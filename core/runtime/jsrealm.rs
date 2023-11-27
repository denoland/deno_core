// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use super::bindings;
use crate::error::exception_to_err_result;
use crate::module_specifier::ModuleSpecifier;
use crate::modules::ModuleCode;
use crate::modules::ModuleError;
use crate::modules::ModuleId;
use crate::modules::ModuleMap;
use crate::ops::OpCtx;
use crate::ops::PendingOp;
use crate::runtime::JsRuntimeState;
use crate::JsRuntime;
use anyhow::Error;
use deno_unsync::JoinSet;
use futures::stream::StreamExt;
use futures::Future;
use std::cell::RefCell;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::hash::BuildHasherDefault;
use std::hash::Hasher;
use std::option::Option;
use std::rc::Rc;
use v8::Handle;
use v8::HandleScope;
use v8::Local;

// Hasher used for `unrefed_ops`. Since these are rolling i32, there's no
// need to actually hash them.
#[derive(Default)]
pub(crate) struct IdentityHasher(u64);

impl Hasher for IdentityHasher {
  fn write_i32(&mut self, i: i32) {
    self.0 = i as u64;
  }

  fn finish(&self) -> u64 {
    self.0
  }

  fn write(&mut self, _bytes: &[u8]) {
    unreachable!()
  }
}

#[derive(Default)]
pub(crate) struct ContextState {
  pub(crate) js_event_loop_tick_cb: Option<Rc<v8::Global<v8::Function>>>,
  pub(crate) js_build_custom_error_cb: Option<Rc<v8::Global<v8::Function>>>,
  pub(crate) js_promise_reject_cb: Option<Rc<v8::Global<v8::Function>>>,
  pub(crate) js_format_exception_cb: Option<Rc<v8::Global<v8::Function>>>,
  pub(crate) js_wasm_streaming_cb: Option<Rc<v8::Global<v8::Function>>>,
  pub(crate) pending_promise_rejections:
    VecDeque<(v8::Global<v8::Promise>, v8::Global<v8::Value>)>,
  pub(crate) unrefed_ops: HashSet<i32, BuildHasherDefault<IdentityHasher>>,
  pub(crate) pending_ops: JoinSet<PendingOp>,
  // We don't explicitly re-read this prop but need the slice to live alongside
  // the context
  pub(crate) op_ctxs: Box<[OpCtx]>,
  pub(crate) isolate: Option<*mut v8::OwnedIsolate>,
  pub(crate) has_next_tick_scheduled: bool,
}

/// A representation of a JavaScript realm tied to a [`JsRuntime`], that allows
/// execution in the realm's context.
///
/// A [`JsRealm`] instance is a reference to an already existing realm, which
/// does not hold ownership of it, so instances can be created and dropped as
/// needed. As such, calling [`JsRealm::new`] doesn't create a new realm, and
/// cloning a [`JsRealm`] only creates a new reference. See
/// [`JsRuntime::create_realm`] to create new realms instead.
///
/// Despite [`JsRealm`] instances being references, multiple instances that
/// point to the same realm won't overlap because every operation requires
/// passing a mutable reference to the [`v8::Isolate`]. Therefore, no operation
/// on two [`JsRealm`] instances tied to the same isolate can be run at the same
/// time, regardless of whether they point to the same realm.
///
/// # Panics
///
/// Every method of [`JsRealm`] will panic if you call it with a reference to a
/// [`v8::Isolate`] other than the one that corresponds to the current context.
///
/// In other words, the [`v8::Isolate`] parameter for all the related [`JsRealm`] methods
/// must be extracted from the pre-existing [`JsRuntime`].
///
/// # Lifetime of the realm
///
/// As long as the corresponding isolate is alive, a [`JsRealm`] instance will
/// keep the underlying V8 context alive even if it would have otherwise been
/// garbage collected.
#[derive(Clone)]
#[repr(transparent)]
pub(crate) struct JsRealm(pub(crate) JsRealmInner);

#[derive(Clone)]
pub(crate) struct JsRealmInner {
  pub(crate) context_state: Rc<RefCell<ContextState>>,
  context: Rc<v8::Global<v8::Context>>,
  pub(crate) module_map: Rc<ModuleMap>,
  runtime_state: Rc<JsRuntimeState>,
}

impl JsRealmInner {
  pub(crate) fn new(
    context_state: Rc<RefCell<ContextState>>,
    context: v8::Global<v8::Context>,
    module_map: Rc<ModuleMap>,
    runtime_state: Rc<JsRuntimeState>,
  ) -> Self {
    Self {
      context_state,
      context: context.into(),
      module_map,
      runtime_state,
    }
  }

  #[inline(always)]
  pub fn context(&self) -> &v8::Global<v8::Context> {
    &self.context
  }

  #[inline(always)]
  pub(crate) fn state(&self) -> Rc<RefCell<ContextState>> {
    self.context_state.clone()
  }

  #[inline(always)]
  pub(crate) fn module_map(&self) -> Rc<ModuleMap> {
    self.module_map.clone()
  }

  /// For info on the [`v8::Isolate`] parameter, check [`JsRealm#panics`].
  #[inline(always)]
  pub fn handle_scope<'s>(
    &self,
    isolate: &'s mut v8::Isolate,
  ) -> v8::HandleScope<'s> {
    v8::HandleScope::with_context(isolate, &*self.context)
  }

  pub(crate) fn check_promise_rejections(
    &self,
    scope: &mut v8::HandleScope,
  ) -> Result<(), Error> {
    let Some((_, handle)) = self
      .context_state
      .borrow_mut()
      .pending_promise_rejections
      .pop_front()
    else {
      return Ok(());
    };

    let exception = v8::Local::new(scope, handle);
    let state = JsRuntime::state_from(scope);
    if let Some(true) = state.with_inspector(|inspector| {
      inspector.exception_thrown(scope, exception, true);
      inspector.has_blocking_sessions()
    }) {
      return Ok(());
    }
    exception_to_err_result(scope, exception, true)
  }

  pub fn destroy(self) {
    let state = self.state();
    let raw_ptr = self.state().borrow().isolate.unwrap();
    // SAFETY: We know the isolate outlives the realm
    let isolate = unsafe { raw_ptr.as_mut().unwrap() };
    let mut realm_state = state.borrow_mut();
    // These globals will prevent snapshots from completing, take them
    std::mem::take(&mut realm_state.js_event_loop_tick_cb);
    std::mem::take(&mut realm_state.js_build_custom_error_cb);
    std::mem::take(&mut realm_state.js_promise_reject_cb);
    std::mem::take(&mut realm_state.js_format_exception_cb);
    std::mem::take(&mut realm_state.js_wasm_streaming_cb);
    // The OpCtx slice may contain a circular reference
    std::mem::take(&mut realm_state.op_ctxs);

    self.context().open(isolate).clear_all_slots(isolate);

    // Expect that this context is dead (we only check this in debug mode)
    // TODO(mmastrac): This check fails for some tests, will need to fix this
    // debug_assert_eq!(Rc::strong_count(&self.context), 1, "Realm was still alive when we wanted to destroy it. Not dropped?");
  }
}

impl JsRealm {
  pub(crate) fn new(inner: JsRealmInner) -> Self {
    Self(inner)
  }

  #[inline(always)]
  pub(crate) fn state_from_scope(
    scope: &mut v8::HandleScope,
  ) -> Rc<RefCell<ContextState>> {
    let context = scope.get_current_context();
    context
      .get_slot::<Rc<RefCell<ContextState>>>(scope)
      .unwrap()
      .clone()
  }

  #[inline(always)]
  pub(crate) fn module_map_from(scope: &mut v8::HandleScope) -> Rc<ModuleMap> {
    let context = scope.get_current_context();
    context.get_slot::<Rc<ModuleMap>>(scope).unwrap().clone()
  }

  #[cfg(test)]
  #[inline(always)]
  pub fn num_pending_ops(&self) -> usize {
    self.0.context_state.borrow().pending_ops.len()
  }

  #[cfg(test)]
  #[inline(always)]
  pub fn num_unrefed_ops(&self) -> usize {
    self.0.context_state.borrow().unrefed_ops.len()
  }

  /// For info on the [`v8::Isolate`] parameter, check [`JsRealm#panics`].
  #[inline(always)]
  pub fn handle_scope<'s>(
    &self,
    isolate: &'s mut v8::Isolate,
  ) -> v8::HandleScope<'s> {
    self.0.handle_scope(isolate)
  }

  #[inline(always)]
  pub fn context(&self) -> &v8::Global<v8::Context> {
    self.0.context()
  }

  pub(crate) fn context_ptr(&self) -> *mut v8::Context {
    unsafe { self.0.context.get_unchecked() as *const _ as _ }
  }

  fn string_from_code<'a>(
    scope: &mut HandleScope<'a>,
    code: &ModuleCode,
  ) -> Option<Local<'a, v8::String>> {
    if let Some(code) = code.try_static_ascii() {
      v8::String::new_external_onebyte_static(scope, code)
    } else {
      v8::String::new_from_utf8(
        scope,
        code.as_bytes(),
        v8::NewStringType::Normal,
      )
    }
  }

  /// Executes traditional JavaScript code (traditional = not ES modules) in the
  /// realm's context.
  ///
  /// For info on the [`v8::Isolate`] parameter, check [`JsRealm#panics`].
  ///
  /// The `name` parameter can be a filepath or any other string. E.g.:
  ///
  ///   - "/some/file/path.js"
  ///   - "<anon>"
  ///   - "[native code]"
  ///
  /// The same `name` value can be used for multiple executions.
  ///
  /// `Error` can usually be downcast to `JsError`.
  #[cfg(test)]
  pub fn execute_script_static(
    &self,
    isolate: &mut v8::Isolate,
    name: &'static str,
    source_code: &'static str,
  ) -> Result<v8::Global<v8::Value>, Error> {
    self.execute_script(isolate, name, ModuleCode::from_static(source_code))
  }

  /// Executes traditional JavaScript code (traditional = not ES modules) in the
  /// realm's context.
  ///
  /// For info on the [`v8::Isolate`] parameter, check [`JsRealm#panics`].
  ///
  /// The `name` parameter can be a filepath or any other string. E.g.:
  ///
  ///   - "/some/file/path.js"
  ///   - "<anon>"
  ///   - "[native code]"
  ///
  /// The same `name` value can be used for multiple executions.
  ///
  /// `Error` can usually be downcast to `JsError`.
  pub fn execute_script(
    &self,
    isolate: &mut v8::Isolate,
    name: &'static str,
    source_code: ModuleCode,
  ) -> Result<v8::Global<v8::Value>, Error> {
    let scope = &mut self.0.handle_scope(isolate);

    let source = Self::string_from_code(scope, &source_code).unwrap();
    debug_assert!(name.is_ascii());
    let name =
      v8::String::new_external_onebyte_static(scope, name.as_bytes()).unwrap();
    let origin = bindings::script_origin(scope, name);

    let tc_scope = &mut v8::TryCatch::new(scope);

    let script = match v8::Script::compile(tc_scope, source, Some(&origin)) {
      Some(script) => script,
      None => {
        let exception = tc_scope.exception().unwrap();
        return exception_to_err_result(tc_scope, exception, false);
      }
    };

    match script.run(tc_scope) {
      Some(value) => {
        let value_handle = v8::Global::new(tc_scope, value);
        Ok(value_handle)
      }
      None => {
        assert!(tc_scope.has_caught());
        let exception = tc_scope.exception().unwrap();
        exception_to_err_result(tc_scope, exception, false)
      }
    }
  }

  /// Returns the namespace object of a module.
  ///
  /// This is only available after module evaluation has completed.
  /// This function panics if module has not been instantiated.
  pub fn get_module_namespace(
    &self,
    isolate: &mut v8::Isolate,
    module_id: ModuleId,
  ) -> Result<v8::Global<v8::Object>, Error> {
    self
      .0
      .module_map()
      .get_module_namespace(&mut self.handle_scope(isolate), module_id)
  }

  pub(crate) fn instantiate_module(
    &self,
    scope: &mut v8::HandleScope,
    id: ModuleId,
  ) -> Result<(), v8::Global<v8::Value>> {
    self.0.module_map().instantiate_module(scope, id)
  }

  /// Evaluates an already instantiated ES module.
  ///
  /// Returns a future that resolves when module promise resolves.
  /// Implementors must manually call [`JsRuntime::run_event_loop`] to drive
  /// module evaluation future.
  ///
  /// `Error` can usually be downcast to `JsError` and should be awaited and
  /// checked after [`JsRuntime::run_event_loop`] completion.
  ///
  /// This function panics if module has not been instantiated.
  pub fn mod_evaluate(
    &self,
    scope: &mut v8::HandleScope,
    id: ModuleId,
  ) -> impl Future<Output = Result<(), Error>> + Unpin {
    self.0.module_map.mod_evaluate(
      scope,
      self.0.context_state.clone(),
      self.0.runtime_state.clone(),
      id,
    )
  }

  pub(crate) fn modules_idle(&self) -> bool {
    self.0.module_map.dyn_module_evaluate_idle_counter.get() > 1
  }

  pub(crate) fn increment_modules_idle(&self) {
    let count = &self.0.module_map.dyn_module_evaluate_idle_counter;
    count.set(count.get() + 1)
  }

  /// Asynchronously load specified module and all of its dependencies.
  ///
  /// The module will be marked as "main", and because of that
  /// "import.meta.main" will return true when checked inside that module.
  ///
  /// User must call [`JsRealm::mod_evaluate`] with returned `ModuleId`
  /// manually after load is finished.
  pub(crate) async fn load_main_module(
    &self,
    isolate: &mut v8::Isolate,
    specifier: &ModuleSpecifier,
    code: Option<ModuleCode>,
  ) -> Result<ModuleId, Error> {
    let module_map_rc = self.0.module_map();
    if let Some(code) = code {
      let specifier = specifier.as_str().to_owned().into();
      let scope = &mut self.handle_scope(isolate);
      // true for main module
      module_map_rc
        .new_es_module(scope, true, specifier, code, false)
        .map_err(|e| match e {
          ModuleError::Exception(exception) => {
            let exception = v8::Local::new(scope, exception);
            exception_to_err_result::<()>(scope, exception, false).unwrap_err()
          }
          ModuleError::Other(error) => error,
        })?;
    }

    let mut load =
      ModuleMap::load_main(module_map_rc.clone(), &specifier).await?;

    while let Some(load_result) = load.next().await {
      let (request, info) = load_result?;
      let scope = &mut self.handle_scope(isolate);
      load.register_and_recurse(scope, &request, info).map_err(
        |e| match e {
          ModuleError::Exception(exception) => {
            let exception = v8::Local::new(scope, exception);
            exception_to_err_result::<()>(scope, exception, false).unwrap_err()
          }
          ModuleError::Other(error) => error,
        },
      )?;
    }

    let root_id = load.root_module_id.expect("Root module should be loaded");
    let scope = &mut self.handle_scope(isolate);
    self.instantiate_module(scope, root_id).map_err(|e| {
      let exception = v8::Local::new(scope, e);
      exception_to_err_result::<()>(scope, exception, false).unwrap_err()
    })?;
    Ok(root_id)
  }

  /// Asynchronously load specified ES module and all of its dependencies.
  ///
  /// This method is meant to be used when loading some utility code that
  /// might be later imported by the main module (ie. an entry point module).
  ///
  /// User must call [`JsRealm::mod_evaluate`] with returned `ModuleId`
  /// manually after load is finished.
  pub(crate) async fn load_side_module(
    &self,
    isolate: &mut v8::Isolate,
    specifier: &ModuleSpecifier,
    code: Option<ModuleCode>,
  ) -> Result<ModuleId, Error> {
    let module_map_rc = self.0.module_map();
    if let Some(code) = code {
      let specifier = specifier.as_str().to_owned().into();
      let scope = &mut self.handle_scope(isolate);
      // false for side module (not main module)
      module_map_rc
        .new_es_module(scope, false, specifier, code, false)
        .map_err(|e| match e {
          ModuleError::Exception(exception) => {
            let exception = v8::Local::new(scope, exception);
            exception_to_err_result::<()>(scope, exception, false).unwrap_err()
          }
          ModuleError::Other(error) => error,
        })?;
    }

    let mut load =
      ModuleMap::load_side(module_map_rc.clone(), &specifier).await?;

    while let Some(load_result) = load.next().await {
      let (request, info) = load_result?;
      let scope = &mut self.handle_scope(isolate);
      load.register_and_recurse(scope, &request, info).map_err(
        |e| match e {
          ModuleError::Exception(exception) => {
            let exception = v8::Local::new(scope, exception);
            exception_to_err_result::<()>(scope, exception, false).unwrap_err()
          }
          ModuleError::Other(error) => error,
        },
      )?;
    }

    let root_id = load.root_module_id.expect("Root module should be loaded");
    let scope = &mut self.handle_scope(isolate);
    self.instantiate_module(scope, root_id).map_err(|e| {
      let exception = v8::Local::new(scope, e);
      exception_to_err_result::<()>(scope, exception, false).unwrap_err()
    })?;
    Ok(root_id)
  }
}
