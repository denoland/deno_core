// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use super::bindings;
use super::exception_state::ExceptionState;
use super::op_driver::OpDriver;
use crate::error::exception_to_err_result;
use crate::module_specifier::ModuleSpecifier;
use crate::modules::ModuleCodeString;
use crate::modules::ModuleId;
use crate::modules::ModuleMap;
use crate::ops::OpCtx;
use crate::tasks::V8TaskSpawnerFactory;
use crate::web_timeout::WebTimers;
use crate::GetErrorClassFn;
use anyhow::Error;
use futures::stream::StreamExt;
use std::cell::Cell;
use std::cell::RefCell;
use std::collections::HashSet;
use std::hash::BuildHasherDefault;
use std::hash::Hasher;
use std::option::Option;
use std::rc::Rc;
use std::sync::Arc;
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

/// We will be experimenting with different driver types in the future. This allows us to
/// swap the driver out for experimentation.
#[cfg(feature = "op_driver_joinset")]
type DefaultOpDriver = super::op_driver::JoinSetDriver;

#[cfg(all(
  feature = "op_driver_futuresunordered",
  not(feature = "op_driver_joinset")
))]
type DefaultOpDriver = super::op_driver::FuturesUnorderedDriver;

pub(crate) struct ContextState<OpDriverImpl: OpDriver = DefaultOpDriver> {
  pub(crate) task_spawner_factory: Arc<V8TaskSpawnerFactory>,
  pub(crate) timers: WebTimers<(v8::Global<v8::Function>, u32)>,
  pub(crate) js_event_loop_tick_cb:
    RefCell<Option<Rc<v8::Global<v8::Function>>>>,
  pub(crate) js_wasm_streaming_cb:
    RefCell<Option<Rc<v8::Global<v8::Function>>>>,
  pub(crate) web_assembly_module_imports_fn:
    RefCell<Option<Rc<v8::Global<v8::Function>>>>,
  pub(crate) web_assembly_module_exports_fn:
    RefCell<Option<Rc<v8::Global<v8::Function>>>>,
  pub(crate) unrefed_ops:
    RefCell<HashSet<i32, BuildHasherDefault<IdentityHasher>>>,
  pub(crate) pending_ops: OpDriverImpl,
  // We don't explicitly re-read this prop but need the slice to live alongside
  // the context
  pub(crate) op_ctxs: RefCell<Box<[OpCtx]>>,
  pub(crate) isolate: Option<*mut v8::OwnedIsolate>,
  pub(crate) exception_state: Rc<ExceptionState>,
  pub(crate) has_next_tick_scheduled: Cell<bool>,
  pub(crate) get_error_class_fn: GetErrorClassFn,
  pub(crate) ops_with_metrics: Vec<bool>,
}

impl<O: OpDriver> ContextState<O> {
  pub(crate) fn new(
    isolate_ptr: *mut v8::OwnedIsolate,
    get_error_class_fn: GetErrorClassFn,
    ops_with_metrics: Vec<bool>,
  ) -> Self {
    Self {
      isolate: Some(isolate_ptr),
      get_error_class_fn,
      ops_with_metrics,
      exception_state: Default::default(),
      has_next_tick_scheduled: Default::default(),
      js_event_loop_tick_cb: Default::default(),
      js_wasm_streaming_cb: Default::default(),
      web_assembly_module_imports_fn: Default::default(),
      web_assembly_module_exports_fn: Default::default(),
      op_ctxs: Default::default(),
      pending_ops: Default::default(),
      task_spawner_factory: Default::default(),
      timers: Default::default(),
      unrefed_ops: Default::default(),
    }
  }
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
  pub(crate) context_state: Rc<ContextState>,
  context: Rc<v8::Global<v8::Context>>,
  pub(crate) module_map: Rc<ModuleMap>,
}

impl JsRealmInner {
  pub(crate) fn new(
    context_state: Rc<ContextState>,
    context: v8::Global<v8::Context>,
    module_map: Rc<ModuleMap>,
  ) -> Self {
    Self {
      context_state,
      context: context.into(),
      module_map,
    }
  }

  #[inline(always)]
  pub fn context(&self) -> &v8::Global<v8::Context> {
    &self.context
  }

  #[inline(always)]
  pub(crate) fn state(&self) -> Rc<ContextState> {
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

  pub fn destroy(self) {
    let state = self.state();
    let raw_ptr = self.state().isolate.unwrap();
    // SAFETY: We know the isolate outlives the realm
    let isolate = unsafe { raw_ptr.as_mut().unwrap() };
    // These globals will prevent snapshots from completing, take them
    state.exception_state.prepare_to_destroy();
    std::mem::take(&mut *state.js_event_loop_tick_cb.borrow_mut());
    std::mem::take(&mut *state.js_wasm_streaming_cb.borrow_mut());
    // The OpCtx slice may contain a circular reference
    std::mem::take(&mut *state.op_ctxs.borrow_mut());

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
  ) -> Rc<ContextState> {
    let context = scope.get_current_context();
    context.get_slot::<Rc<ContextState>>(scope).unwrap().clone()
  }

  #[inline(always)]
  pub(crate) fn module_map_from(scope: &mut v8::HandleScope) -> Rc<ModuleMap> {
    let context = scope.get_current_context();
    context.get_slot::<Rc<ModuleMap>>(scope).unwrap().clone()
  }

  #[inline(always)]
  pub(crate) fn exception_state_from_scope(
    scope: &mut v8::HandleScope,
  ) -> Rc<ExceptionState> {
    let context = scope.get_current_context();
    context
      .get_slot::<Rc<ContextState>>(scope)
      .unwrap()
      .exception_state
      .clone()
  }

  #[cfg(test)]
  #[inline(always)]
  pub fn num_pending_ops(&self) -> usize {
    self.0.context_state.pending_ops.len()
  }

  #[cfg(test)]
  #[inline(always)]
  pub fn num_unrefed_ops(&self) -> usize {
    self.0.context_state.unrefed_ops.borrow().len()
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
    code: &ModuleCodeString,
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
    self.execute_script(
      isolate,
      name,
      ModuleCodeString::from_static(source_code),
    )
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
    source_code: ModuleCodeString,
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
        return exception_to_err_result(tc_scope, exception, false, false);
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
        exception_to_err_result(tc_scope, exception, false, false)
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
  /// User must call [`ModuleMap::mod_evaluate`] with returned `ModuleId`
  /// manually after load is finished.
  pub(crate) async fn load_main_module(
    &self,
    isolate: &mut v8::Isolate,
    specifier: &ModuleSpecifier,
    code: Option<ModuleCodeString>,
  ) -> Result<ModuleId, Error> {
    let module_map_rc = self.0.module_map();
    if let Some(code) = code {
      let specifier = specifier.as_str().to_owned().into();
      let scope = &mut self.handle_scope(isolate);
      // true for main module
      module_map_rc
        .new_es_module(scope, true, specifier, code, false)
        .map_err(|e| e.into_any_error(scope, false, false))?;
    }

    let mut load =
      ModuleMap::load_main(module_map_rc.clone(), &specifier).await?;

    while let Some(load_result) = load.next().await {
      let (request, info) = load_result?;
      let scope = &mut self.handle_scope(isolate);
      load
        .register_and_recurse(scope, &request, info)
        .map_err(|e| e.into_any_error(scope, false, false))?;
    }

    let root_id = load.root_module_id.expect("Root module should be loaded");
    let scope = &mut self.handle_scope(isolate);
    self.instantiate_module(scope, root_id).map_err(|e| {
      let exception = v8::Local::new(scope, e);
      exception_to_err_result::<()>(scope, exception, false, false).unwrap_err()
    })?;
    Ok(root_id)
  }

  /// Asynchronously load specified ES module and all of its dependencies.
  ///
  /// This method is meant to be used when loading some utility code that
  /// might be later imported by the main module (ie. an entry point module).
  ///
  /// User must call [`ModuleMap::mod_evaluate`] with returned `ModuleId`
  /// manually after load is finished.
  pub(crate) async fn load_side_module(
    &self,
    isolate: &mut v8::Isolate,
    specifier: &ModuleSpecifier,
    code: Option<ModuleCodeString>,
  ) -> Result<ModuleId, Error> {
    let module_map_rc = self.0.module_map();
    if let Some(code) = code {
      let specifier = specifier.as_str().to_owned().into();
      let scope = &mut self.handle_scope(isolate);
      // false for side module (not main module)
      module_map_rc
        .new_es_module(scope, false, specifier, code, false)
        .map_err(|e| e.into_any_error(scope, false, false))?;
    }

    let mut load =
      ModuleMap::load_side(module_map_rc.clone(), &specifier).await?;

    while let Some(load_result) = load.next().await {
      let (request, info) = load_result?;
      let scope = &mut self.handle_scope(isolate);
      load
        .register_and_recurse(scope, &request, info)
        .map_err(|e| e.into_any_error(scope, false, false))?;
    }

    let root_id = load.root_module_id.expect("Root module should be loaded");
    let scope = &mut self.handle_scope(isolate);
    self.instantiate_module(scope, root_id).map_err(|e| {
      let exception = v8::Local::new(scope, e);
      exception_to_err_result::<()>(scope, exception, false, false).unwrap_err()
    })?;
    Ok(root_id)
  }

  /// Load and evaluate an ES module provided the specifier and source code.
  ///
  /// The module should not have Top-Level Await (that is, it should be
  /// possible to evaluate it synchronously).
  ///
  /// It is caller's responsibility to ensure that not duplicate specifiers are
  /// passed to this method.
  pub(crate) fn lazy_load_es_module_from_code(
    &self,
    isolate: &mut v8::Isolate,
    module_specifier: &str,
    code: ModuleCodeString,
  ) -> Result<v8::Global<v8::Value>, Error> {
    let module_map_rc = self.0.module_map();
    let scope = &mut self.handle_scope(isolate);
    module_map_rc.lazy_load_es_module_from_code(scope, module_specifier, code)
  }
}
