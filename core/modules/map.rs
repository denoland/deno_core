// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use crate::error::exception_to_err_result;
use crate::error::generic_error;
use crate::error::throw_type_error;
use crate::error::to_v8_type_error;
use crate::error::AnyError;
use crate::error::JsError;
use crate::modules::get_requested_module_type_from_attributes;
use crate::modules::parse_import_attributes;
use crate::modules::recursive_load::RecursiveModuleLoad;
use crate::modules::ImportAttributesKind;
use crate::modules::ModuleCodeString;
use crate::modules::ModuleError;
use crate::modules::ModuleId;
use crate::modules::ModuleLoadId;
use crate::modules::ModuleLoader;
use crate::modules::ModuleName;
use crate::modules::ModuleRequest;
use crate::modules::ModuleType;
use crate::modules::ResolutionKind;
use crate::runtime::exception_state::ExceptionState;
use crate::runtime::JsRealm;
use crate::ExtensionFileSource;
use crate::JsRuntime;
use crate::ModuleSource;
use crate::ModuleSpecifier;
use anyhow::bail;
use anyhow::Error;
use futures::future::FutureExt;
use futures::stream::FuturesUnordered;
use futures::stream::StreamFuture;
use futures::task::AtomicWaker;
use futures::Future;
use futures::StreamExt;
use log::debug;
use v8::Function;
use v8::PromiseState;

use std::cell::Cell;
use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::DerefMut;
use std::pin::Pin;
use std::rc::Rc;
use std::task::Context;
use std::task::Poll;
use tokio::sync::oneshot;

use super::module_map_data::ModuleMapData;
use super::module_map_data::ModuleMapSnapshottedData;
use super::LazyEsmModuleLoader;
use super::RequestedModuleType;

type PrepareLoadFuture =
  dyn Future<Output = (ModuleLoadId, Result<RecursiveModuleLoad, Error>)>;

use super::ImportMetaResolveCallback;

struct ModEvaluate {
  module_map: Rc<ModuleMap>,
  sender: Option<oneshot::Sender<Result<(), Error>>>,
}

pub const BOM_CHAR: &[u8] = &[0xef, 0xbb, 0xbf];

/// Strips the byte order mark from the provided text if it exists.
fn strip_bom(source_code: &[u8]) -> &[u8] {
  if source_code.starts_with(BOM_CHAR) {
    &source_code[BOM_CHAR.len()..]
  } else {
    source_code
  }
}

struct DynImportModEvaluate {
  load_id: ModuleLoadId,
  module_id: ModuleId,
  promise: v8::Global<v8::Promise>,
  module: v8::Global<v8::Module>,
}

/// A collection of JS modules.
pub(crate) struct ModuleMap {
  // Handling of futures for loading module sources
  // TODO(mmastrac): we should not be swapping this loader out
  pub(crate) loader: RefCell<Rc<dyn ModuleLoader>>,
  pub(crate) import_meta_resolve_cb: ImportMetaResolveCallback,

  exception_state: Rc<ExceptionState>,
  dynamic_import_map:
    RefCell<HashMap<ModuleLoadId, v8::Global<v8::PromiseResolver>>>,
  preparing_dynamic_imports:
    RefCell<FuturesUnordered<Pin<Box<PrepareLoadFuture>>>>,
  preparing_dynamic_imports_pending: Cell<bool>,
  pending_dynamic_imports:
    RefCell<FuturesUnordered<StreamFuture<RecursiveModuleLoad>>>,
  pending_dynamic_imports_pending: Cell<bool>,
  pending_dyn_mod_evaluations: RefCell<Vec<DynImportModEvaluate>>,
  pending_dyn_mod_evaluations_pending: Cell<bool>,
  pending_mod_evaluation: Cell<bool>,
  module_waker: AtomicWaker,
  data: RefCell<ModuleMapData>,

  /// A counter used to delay our dynamic import deadlock detection by one spin
  /// of the event loop.
  pub(crate) dyn_module_evaluate_idle_counter: Cell<u32>,
}

impl ModuleMap {
  pub(crate) fn next_load_id(&self) -> i32 {
    // TODO(mmastrac): move recursive module loading into here so we can avoid making this pub
    let mut data = self.data.borrow_mut();
    let id = data.next_load_id;
    data.next_load_id += 1;
    id + 1
  }

  #[cfg(debug_assertions)]
  pub(crate) fn check_all_modules_evaluated(
    &self,
    scope: &mut v8::HandleScope,
  ) -> Result<(), Error> {
    let mut not_evaluated = vec![];
    let data = self.data.borrow();

    for (handle, i) in data.handles_inverted.iter() {
      let module = v8::Local::new(scope, handle);
      match module.get_status() {
        v8::ModuleStatus::Errored => {
          return Err(
            JsError::from_v8_exception(scope, module.get_exception()).into(),
          );
        }
        v8::ModuleStatus::Evaluated => {}
        _ => {
          not_evaluated.push(data.info[*i].name.as_str().to_string());
        }
      }
    }

    if !not_evaluated.is_empty() {
      let mut msg = String::new();
      for m in not_evaluated {
        msg.push_str(&format!("  - {}\n", m));
      }
      bail!("Following modules were not evaluated; make sure they are imported from other code:\n {}", msg);
    }

    Ok(())
  }

  pub(crate) fn new(
    loader: Rc<dyn ModuleLoader>,
    exception_state: Rc<ExceptionState>,
    import_meta_resolve_cb: ImportMetaResolveCallback,
  ) -> Self {
    Self {
      loader: loader.into(),
      exception_state,
      import_meta_resolve_cb,
      dyn_module_evaluate_idle_counter: Default::default(),
      dynamic_import_map: Default::default(),
      preparing_dynamic_imports: Default::default(),
      preparing_dynamic_imports_pending: Default::default(),
      pending_dynamic_imports: Default::default(),
      pending_dynamic_imports_pending: Default::default(),
      pending_dyn_mod_evaluations: Default::default(),
      pending_dyn_mod_evaluations_pending: Default::default(),
      pending_mod_evaluation: Default::default(),
      module_waker: Default::default(),
      data: Default::default(),
    }
  }

  pub(crate) fn new_from_snapshotted_data(
    loader: Rc<dyn ModuleLoader>,
    exception_state: Rc<ExceptionState>,
    import_meta_resolve_cb: ImportMetaResolveCallback,
    scope: &mut v8::HandleScope,
    data: ModuleMapSnapshottedData,
  ) -> Self {
    let new = Self::new(loader, exception_state, import_meta_resolve_cb);
    new
      .data
      .borrow_mut()
      .update_with_snapshotted_data(scope, data);
    new
  }

  fn get_handle_by_name(
    &self,
    name: impl AsRef<str>,
  ) -> Option<v8::Global<v8::Module>> {
    let id = self
      .get_id(name.as_ref(), RequestedModuleType::None)
      .or_else(|| self.get_id(name.as_ref(), RequestedModuleType::Json))?;
    self.get_handle(id)
  }

  /// Get module id, following all aliases in case of module specifier
  /// that had been redirected.
  pub(crate) fn get_id(
    &self,
    name: impl AsRef<str>,
    requested_module_type: impl AsRef<RequestedModuleType>,
  ) -> Option<ModuleId> {
    self.data.borrow().get_id(name, requested_module_type)
  }

  pub(crate) fn is_main_module(&self, global: &v8::Global<v8::Module>) -> bool {
    self.data.borrow().is_main_module(global)
  }

  pub(crate) fn get_name_by_module(
    &self,
    global: &v8::Global<v8::Module>,
  ) -> Option<String> {
    self.data.borrow().get_name_by_module(global)
  }

  pub(crate) fn get_name_by_id(&self, id: ModuleId) -> Option<String> {
    self.data.borrow().get_name_by_id(id)
  }

  pub(crate) fn get_handle(
    &self,
    id: ModuleId,
  ) -> Option<v8::Global<v8::Module>> {
    self.data.borrow().get_handle(id)
  }

  pub(crate) fn serialize_for_snapshotting(
    &self,
    scope: &mut v8::HandleScope,
  ) -> ModuleMapSnapshottedData {
    self.data.borrow().serialize_for_snapshotting(scope)
  }

  #[cfg(test)]
  pub fn is_alias(
    &self,
    name: &str,
    requested_module_type: impl AsRef<RequestedModuleType>,
  ) -> bool {
    self.data.borrow().is_alias(name, requested_module_type)
  }

  #[cfg(test)]
  pub fn assert_module_map(&self, modules: &Vec<super::ModuleInfo>) {
    self.data.borrow().assert_module_map(modules);
  }

  pub(crate) fn new_module(
    &self,
    scope: &mut v8::HandleScope,
    main: bool,
    dynamic: bool,
    module_source: ModuleSource,
  ) -> Result<ModuleId, ModuleError> {
    let ModuleSource {
      code,
      module_type,
      module_url_found,
      module_url_specified,
    } = module_source;

    // Register the module in the module map unless it's already there. If the
    // specified URL and the "true" URL are different, register the alias.
    let module_url_found = if let Some(module_url_found) = module_url_found {
      let (module_url_found1, module_url_found2) =
        module_url_found.into_cheap_copy();
      self.data.borrow_mut().alias(
        module_url_specified,
        &module_type.into(),
        module_url_found1,
      );
      module_url_found2
    } else {
      module_url_specified
    };

    let requested_module_type = RequestedModuleType::from(module_type);
    let maybe_module_id = self.get_id(&module_url_found, requested_module_type);

    if let Some(module_id) = maybe_module_id {
      debug!(
        "Already-registered module fetched again: {:?}",
        module_url_found
      );
      return Ok(module_id);
    }

    let code = ModuleSource::get_string_source(module_url_found.as_str(), code)
      .map_err(ModuleError::Other)?;
    let module_id = match module_type {
      ModuleType::JavaScript => {
        self.new_es_module(scope, main, module_url_found, code, dynamic)?
      }
      ModuleType::Json => {
        self.new_json_module(scope, module_url_found, code)?
      }
    };
    Ok(module_id)
  }

  /// Creates a "synthetic module", that contains only a single, "default" export,
  /// that returns the provided value.
  pub fn new_synthetic_module(
    &self,
    scope: &mut v8::HandleScope,
    name: ModuleName,
    module_type: ModuleType,
    value: v8::Local<v8::Value>,
  ) -> Result<ModuleId, ModuleError> {
    let name_str = name.v8(scope);

    let export_names = [v8::String::new(scope, "default").unwrap()];
    let module = v8::Module::create_synthetic_module(
      scope,
      name_str,
      &export_names,
      synthetic_module_evaluation_steps,
    );

    let handle = v8::Global::<v8::Module>::new(scope, module);
    let value_handle = v8::Global::<v8::Value>::new(scope, value);
    self
      .data
      .borrow_mut()
      .synthetic_module_value_store
      .insert(handle.clone(), value_handle);

    let id = self.data.borrow_mut().create_module_info(
      name,
      module_type,
      handle,
      false,
      vec![],
    );

    Ok(id)
  }

  /// Create and compile an ES module.
  pub(crate) fn new_es_module(
    &self,
    scope: &mut v8::HandleScope,
    main: bool,
    name: ModuleName,
    source: ModuleCodeString,
    is_dynamic_import: bool,
  ) -> Result<ModuleId, ModuleError> {
    let name_str = name.v8(scope);
    let source_str = source.v8(scope);

    let origin = module_origin(scope, name_str);
    let source = v8::script_compiler::Source::new(source_str, Some(&origin));

    let tc_scope = &mut v8::TryCatch::new(scope);

    let maybe_module = v8::script_compiler::compile_module(tc_scope, source);

    if tc_scope.has_caught() {
      assert!(maybe_module.is_none());
      let exception = tc_scope.exception().unwrap();
      let exception = v8::Global::new(tc_scope, exception);
      return Err(ModuleError::Exception(exception));
    }

    let module = maybe_module.unwrap();

    let mut requests: Vec<ModuleRequest> = vec![];
    let module_requests = module.get_module_requests();
    for i in 0..module_requests.length() {
      let module_request = v8::Local::<v8::ModuleRequest>::try_from(
        module_requests.get(tc_scope, i).unwrap(),
      )
      .unwrap();
      let import_specifier = module_request
        .get_specifier()
        .to_rust_string_lossy(tc_scope);

      let import_attributes = module_request.get_import_assertions();

      let attributes = parse_import_attributes(
        tc_scope,
        import_attributes,
        ImportAttributesKind::StaticImport,
      );

      // FIXME(bartomieju): there are no stack frames if exception
      // is thrown here
      {
        let state = JsRuntime::state_from(tc_scope);
        (state.validate_import_attributes_cb)(tc_scope, &attributes);
      }

      if tc_scope.has_caught() {
        let exception = tc_scope.exception().unwrap();
        let exception = v8::Global::new(tc_scope, exception);
        return Err(ModuleError::Exception(exception));
      }

      let module_specifier = match self.resolve(
        &import_specifier,
        name.as_ref(),
        if is_dynamic_import {
          ResolutionKind::DynamicImport
        } else {
          ResolutionKind::Import
        },
      ) {
        Ok(s) => s,
        Err(e) => return Err(ModuleError::Other(e)),
      };
      let requested_module_type =
        get_requested_module_type_from_attributes(&attributes);
      let request = ModuleRequest {
        specifier: module_specifier.to_string(),
        requested_module_type,
      };
      requests.push(request);
    }

    if main {
      let data = self.data.borrow();
      if let Some(main_module) = data.main_module_id {
        let main_name = self.data.borrow().get_name_by_id(main_module).unwrap();
        return Err(ModuleError::Other(generic_error(
          format!("Trying to create \"main\" module ({:?}), when one already exists ({:?})",
          name.as_ref(),
          main_name,
        ))));
      }
    }

    let handle = v8::Global::<v8::Module>::new(tc_scope, module);
    let id = self.data.borrow_mut().create_module_info(
      name,
      ModuleType::JavaScript,
      handle,
      main,
      requests,
    );

    Ok(id)
  }

  pub(crate) fn new_json_module(
    &self,
    scope: &mut v8::HandleScope,
    name: ModuleName,
    code: ModuleCodeString,
  ) -> Result<ModuleId, ModuleError> {
    let source_str = v8::String::new_from_utf8(
      scope,
      strip_bom(code.as_bytes()),
      v8::NewStringType::Normal,
    )
    .unwrap();
    let tc_scope = &mut v8::TryCatch::new(scope);

    let parsed_json = match v8::json::parse(tc_scope, source_str) {
      Some(parsed_json) => parsed_json,
      None => {
        assert!(tc_scope.has_caught());
        let exception = tc_scope.exception().unwrap();
        let exception = v8::Global::new(tc_scope, exception);
        return Err(ModuleError::Exception(exception));
      }
    };
    self.new_synthetic_module(tc_scope, name, ModuleType::Json, parsed_json)
  }

  pub(crate) fn instantiate_module(
    &self,
    scope: &mut v8::HandleScope,
    id: ModuleId,
  ) -> Result<(), v8::Global<v8::Value>> {
    let tc_scope = &mut v8::TryCatch::new(scope);

    let module = self
      .get_handle(id)
      .map(|handle| v8::Local::new(tc_scope, handle))
      .expect("ModuleInfo not found");

    if module.get_status() == v8::ModuleStatus::Errored {
      return Err(v8::Global::new(tc_scope, module.get_exception()));
    }

    tc_scope.set_slot(self as *const _);
    let instantiate_result =
      module.instantiate_module(tc_scope, Self::module_resolve_callback);
    tc_scope.remove_slot::<*const Self>();
    if instantiate_result.is_none() {
      let exception = tc_scope.exception().unwrap();
      return Err(v8::Global::new(tc_scope, exception));
    }

    Ok(())
  }

  /// Called by V8 during `JsRuntime::instantiate_module`. This is only used internally, so we use the Isolate's annex
  /// to propagate a &Self.
  fn module_resolve_callback<'s>(
    context: v8::Local<'s, v8::Context>,
    specifier: v8::Local<'s, v8::String>,
    import_attributes: v8::Local<'s, v8::FixedArray>,
    referrer: v8::Local<'s, v8::Module>,
  ) -> Option<v8::Local<'s, v8::Module>> {
    // SAFETY: `CallbackScope` can be safely constructed from `Local<Context>`
    let scope = &mut unsafe { v8::CallbackScope::new(context) };

    let module_map =
      // SAFETY: We retrieve the pointer from the slot, having just set it a few stack frames up
      unsafe { scope.get_slot::<*const Self>().unwrap().as_ref().unwrap() };

    let referrer_global = v8::Global::new(scope, referrer);

    let referrer_name = module_map
      .data
      .borrow()
      .get_name_by_module(&referrer_global)
      .expect("ModuleInfo not found");

    let specifier_str = specifier.to_rust_string_lossy(scope);

    let assertions = parse_import_attributes(
      scope,
      import_attributes,
      ImportAttributesKind::StaticImport,
    );
    let maybe_module = module_map.resolve_callback(
      scope,
      &specifier_str,
      &referrer_name,
      assertions,
    );
    if let Some(module) = maybe_module {
      return Some(module);
    }

    let msg = format!(
      r#"Cannot resolve module "{specifier_str}" from "{referrer_name}""#
    );
    throw_type_error(scope, msg);
    None
  }

  /// Resolve provided module. This function calls out to `loader.resolve`,
  /// but applies some additional checks that disallow resolving/importing
  /// certain modules (eg. `ext:` or `node:` modules)
  pub fn resolve(
    &self,
    specifier: &str,
    referrer: &str,
    kind: ResolutionKind,
  ) -> Result<ModuleSpecifier, AnyError> {
    if specifier.starts_with("ext:")
      && !referrer.starts_with("ext:")
      && !referrer.starts_with("node:")
      && referrer != "."
      && kind != ResolutionKind::MainModule
    {
      let referrer = if referrer.is_empty() {
        "(no referrer)"
      } else {
        referrer
      };
      let msg = format!("Importing ext: modules is only allowed from ext: and node: modules. Tried to import {} from {}", specifier, referrer);
      return Err(generic_error(msg));
    }

    self.loader.borrow().resolve(specifier, referrer, kind)
  }

  /// Called by `module_resolve_callback` during module instantiation.
  fn resolve_callback<'s>(
    &self,
    scope: &mut v8::HandleScope<'s>,
    specifier: &str,
    referrer: &str,
    import_attributes: HashMap<String, String>,
  ) -> Option<v8::Local<'s, v8::Module>> {
    let resolved_specifier =
      match self.resolve(specifier, referrer, ResolutionKind::Import) {
        Ok(s) => s,
        Err(e) => {
          throw_type_error(scope, e.to_string());
          return None;
        }
      };

    let module_type =
      get_requested_module_type_from_attributes(&import_attributes);

    if let Some(id) = self.get_id(resolved_specifier.as_str(), module_type) {
      if let Some(handle) = self.get_handle(id) {
        return Some(v8::Local::new(scope, handle));
      }
    }

    None
  }

  pub(crate) fn inject_handle(
    &self,
    name: ModuleName,
    module_type: ModuleType,
    handle: v8::Global<v8::Module>,
  ) {
    self.data.borrow_mut().create_module_info(
      name,
      module_type,
      handle,
      false,
      vec![],
    );
  }

  pub(crate) fn get_requested_modules(
    &self,
    id: ModuleId,
  ) -> Option<Vec<ModuleRequest>> {
    // TODO(mmastrac): Remove cloning. We were originally cloning this at the call sites but that's no excuse.
    self.data.borrow().info.get(id).map(|i| i.requests.clone())
  }

  pub(crate) async fn load_main(
    module_map_rc: Rc<ModuleMap>,
    specifier: impl AsRef<str>,
  ) -> Result<RecursiveModuleLoad, Error> {
    let load =
      RecursiveModuleLoad::main(specifier.as_ref(), module_map_rc.clone());
    load.prepare().await?;
    Ok(load)
  }

  pub(crate) async fn load_side(
    module_map_rc: Rc<ModuleMap>,
    specifier: impl AsRef<str>,
  ) -> Result<RecursiveModuleLoad, Error> {
    let load =
      RecursiveModuleLoad::side(specifier.as_ref(), module_map_rc.clone());
    load.prepare().await?;
    Ok(load)
  }

  // Initiate loading of a module graph imported using `import()`.
  pub(crate) fn load_dynamic_import(
    self: Rc<Self>,
    specifier: &str,
    referrer: &str,
    requested_module_type: RequestedModuleType,
    resolver_handle: v8::Global<v8::PromiseResolver>,
  ) {
    let load = RecursiveModuleLoad::dynamic_import(
      specifier,
      referrer,
      requested_module_type.clone(),
      self.clone(),
    );

    self
      .dynamic_import_map
      .borrow_mut()
      .insert(load.id, resolver_handle);

    let resolve_result =
      self.resolve(specifier, referrer, ResolutionKind::DynamicImport);
    let fut = match resolve_result {
      Ok(module_specifier) => {
        if self
          .data
          .borrow()
          .is_registered(module_specifier, requested_module_type)
        {
          async move { (load.id, Ok(load)) }.boxed_local()
        } else {
          async move { (load.id, load.prepare().await.map(|()| load)) }
            .boxed_local()
        }
      }
      Err(error) => async move { (load.id, Err(error)) }.boxed_local(),
    };
    self.preparing_dynamic_imports.borrow_mut().push(fut);
    self.preparing_dynamic_imports_pending.set(true);
  }

  pub(crate) fn has_pending_dynamic_imports(&self) -> bool {
    self.preparing_dynamic_imports_pending.get()
      || self.pending_dynamic_imports_pending.get()
  }

  pub(crate) fn has_pending_module_evaluation(&self) -> bool {
    self.pending_mod_evaluation.get()
  }
  pub(crate) fn has_pending_dyn_module_evaluation(&self) -> bool {
    self.pending_dyn_mod_evaluations_pending.get()
  }

  /// See [`JsRuntime::mod_evaluate`].
  pub fn mod_evaluate(
    self: &Rc<Self>,
    scope: &mut v8::HandleScope,
    id: ModuleId,
  ) -> impl Future<Output = Result<(), Error>> + Unpin {
    let tc_scope = &mut v8::TryCatch::new(scope);

    let module = self
      .get_handle(id)
      .map(|handle| v8::Local::new(tc_scope, handle))
      .expect("ModuleInfo not found");
    let mut status = module.get_status();
    assert_eq!(
      status,
      v8::ModuleStatus::Instantiated,
      "{} {} ({})",
      if status == v8::ModuleStatus::Evaluated {
        "Module already evaluated. Perhaps you've re-provided a module or extension that was already included in the snapshot?"
      } else {
        "Module not instantiated"
      },
      self.get_name_by_id(id).unwrap(),
      id,
    );

    let (sender, receiver) = oneshot::channel();
    let receiver = receiver.map(|res| {
      res.unwrap_or_else(|_| {
        bail!("Cannot evaluate module, because JavaScript execution has been terminated")
      }
    )});

    let Some(value) = module.evaluate(tc_scope) else {
      if tc_scope.has_terminated() || tc_scope.is_execution_terminating() {
        let undefined = v8::undefined(tc_scope).into();
        _ = sender
          .send(exception_to_err_result(tc_scope, undefined, true, false));
      } else {
        debug_assert_eq!(module.get_status(), v8::ModuleStatus::Errored);
      }
      return receiver;
    };

    self.pending_mod_evaluation.set(true);

    // Update status after evaluating.
    status = module.get_status();

    if self.exception_state.has_dispatched_exception() {
      // This will be overridden in `exception_to_err_result()`.
      let exception = v8::undefined(tc_scope).into();
      sender
        .send(exception_to_err_result(tc_scope, exception, false, false))
        .expect("Failed to send module evaluation error.");
    } else {
      debug_assert!(
        status == v8::ModuleStatus::Evaluated
          || status == v8::ModuleStatus::Errored
      );
      let promise = v8::Local::<v8::Promise>::try_from(value)
        .expect("Expected to get promise as module evaluation result");

      // Create a ModEvaluate instance and stash it in an external
      let evaluation = v8::External::new(
        tc_scope,
        Box::into_raw(Box::new(ModEvaluate {
          module_map: self.clone(),
          sender: Some(sender),
        })) as _,
      );

      fn get_sender(arg: v8::Local<v8::Value>) -> ModEvaluate {
        let sender = v8::Local::<v8::External>::try_from(arg).unwrap();
        *unsafe { Box::from_raw(sender.value() as _) }
      }

      let on_fulfilled = Function::builder(
        |_scope: &mut v8::HandleScope<'_>,
         args: v8::FunctionCallbackArguments<'_>,
         _rv: v8::ReturnValue| {
          let mut sender = get_sender(args.data());
          sender.module_map.pending_mod_evaluation.set(false);
          sender.module_map.module_waker.wake();
          _ = sender.sender.take().unwrap().send(Ok(()));
        },
      )
      .data(evaluation.into())
      .build(tc_scope);

      let on_rejected = Function::builder(
        |scope: &mut v8::HandleScope<'_>,
         args: v8::FunctionCallbackArguments<'_>,
         _rv: v8::ReturnValue| {
          let mut sender = get_sender(args.data());
          sender.module_map.pending_mod_evaluation.set(false);
          sender.module_map.module_waker.wake();
          _ = sender.sender.take().unwrap().send(Ok(()));
          scope.throw_exception(args.get(0));
        },
      )
      .data(evaluation.into())
      .build(tc_scope);

      // V8 GC roots all promises, so we don't need to worry about it after this
      // then2 will return None if the runtime is shutting down
      if on_fulfilled.is_none()
        || on_rejected.is_none()
        || promise
          .then2(tc_scope, on_fulfilled.unwrap(), on_rejected.unwrap())
          .is_none()
      {
        // The runtime might have been shut down, so we need to synthesize a promise result here
        let mut sender = get_sender(evaluation.into());
        match promise.state() {
          PromiseState::Fulfilled => {
            // Module loaded OK
            _ = sender.sender.take().unwrap().send(Ok(()));
          }
          PromiseState::Rejected => {
            // Module was rejected
            let err = promise.result(tc_scope);
            let err = JsError::from_v8_exception(tc_scope, err);
            _ = sender.sender.take().unwrap().send(Err(err.into()));
          }
          PromiseState::Pending => {
            // Module pending, just drop the sender at this point -- we can't do anything with a shut-down runtime.
            drop(sender);
          }
        }
      }

      tc_scope.perform_microtask_checkpoint();
    }

    receiver
  }

  fn dynamic_import_module_evaluate(
    &self,
    scope: &mut v8::HandleScope,
    load_id: ModuleLoadId,
    id: ModuleId,
  ) -> Result<(), Error> {
    let module_handle = self.get_handle(id).expect("ModuleInfo not found");

    let status = {
      let module = module_handle.open(scope);
      module.get_status()
    };

    match status {
      v8::ModuleStatus::Instantiated | v8::ModuleStatus::Evaluated => {}
      _ => return Ok(()),
    }

    // IMPORTANT: Top-level-await is enabled, which means that return value
    // of module evaluation is a promise.
    //
    // This promise is internal, and not the same one that gets returned to
    // the user. We add an empty `.catch()` handler so that it does not result
    // in an exception if it rejects. That will instead happen for the other
    // promise if not handled by the user.
    //
    // For more details see:
    // https://github.com/denoland/deno/issues/4908
    // https://v8.dev/features/top-level-await#module-execution-order
    let tc_scope = &mut v8::TryCatch::new(scope);
    let module = v8::Local::new(tc_scope, &module_handle);
    let maybe_value = module.evaluate(tc_scope);

    // Update status after evaluating.
    let status = module.get_status();

    if let Some(value) = maybe_value {
      debug_assert!(
        status == v8::ModuleStatus::Evaluated
          || status == v8::ModuleStatus::Errored
      );
      let promise = v8::Local::<v8::Promise>::try_from(value)
        .expect("Expected to get promise as module evaluation result");
      let empty_fn =
        crate::runtime::bindings::create_empty_fn(tc_scope).unwrap();
      promise.catch(tc_scope, empty_fn);
      let promise_global = v8::Global::new(tc_scope, promise);
      let module_global = v8::Global::new(tc_scope, module);

      let dyn_import_mod_evaluate = DynImportModEvaluate {
        load_id,
        module_id: id,
        promise: promise_global,
        module: module_global,
      };

      self
        .pending_dyn_mod_evaluations
        .borrow_mut()
        .push(dyn_import_mod_evaluate);
      self.pending_dyn_mod_evaluations_pending.set(true);
    } else if tc_scope.has_terminated() || tc_scope.is_execution_terminating() {
      return Err(
        generic_error("Cannot evaluate dynamically imported module, because JavaScript execution has been terminated.")
      );
    } else {
      assert_eq!(status, v8::ModuleStatus::Errored);
    }

    Ok(())
  }

  // Returns true if some dynamic import was resolved.
  fn evaluate_dyn_imports(&self, scope: &mut v8::HandleScope) -> bool {
    if !self.pending_dyn_mod_evaluations_pending.get() {
      return false;
    }

    let pending =
      std::mem::take(self.pending_dyn_mod_evaluations.borrow_mut().deref_mut());
    let mut resolved_any = false;
    let mut still_pending = vec![];
    for pending_dyn_evaluate in pending {
      let maybe_result = {
        let module_id = pending_dyn_evaluate.module_id;
        let promise = pending_dyn_evaluate.promise.open(scope);
        let _module = pending_dyn_evaluate.module.open(scope);
        let promise_state = promise.state();

        match promise_state {
          v8::PromiseState::Pending => {
            still_pending.push(pending_dyn_evaluate);
            None
          }
          v8::PromiseState::Fulfilled => {
            Some(Ok((pending_dyn_evaluate.load_id, module_id)))
          }
          v8::PromiseState::Rejected => {
            let exception = promise.result(scope);
            let exception = v8::Global::new(scope, exception);
            Some(Err((pending_dyn_evaluate.load_id, exception)))
          }
        }
      };

      if let Some(result) = maybe_result {
        resolved_any = true;
        match result {
          Ok((dyn_import_id, module_id)) => {
            self.dynamic_import_resolve(scope, dyn_import_id, module_id);
          }
          Err((dyn_import_id, exception)) => {
            self.dynamic_import_reject(scope, dyn_import_id, exception);
          }
        }
      }
    }
    self
      .pending_dyn_mod_evaluations_pending
      .set(!still_pending.is_empty());
    *self.pending_dyn_mod_evaluations.borrow_mut() = still_pending;
    resolved_any
  }

  pub(crate) fn dynamic_import_reject(
    &self,
    scope: &mut v8::HandleScope,
    id: ModuleLoadId,
    exception: v8::Global<v8::Value>,
  ) {
    let resolver_handle = self
      .dynamic_import_map
      .borrow_mut()
      .remove(&id)
      .expect("Invalid dynamic import id");
    let resolver = resolver_handle.open(scope);

    let exception = v8::Local::new(scope, exception);
    resolver.reject(scope, exception).unwrap();
    scope.perform_microtask_checkpoint();
  }

  pub(crate) fn dynamic_import_resolve(
    &self,
    scope: &mut v8::HandleScope,
    id: ModuleLoadId,
    mod_id: ModuleId,
  ) {
    let resolver_handle = self
      .dynamic_import_map
      .borrow_mut()
      .remove(&id)
      .expect("Invalid dynamic import id");
    let resolver = resolver_handle.open(scope);

    let module = self
      .data
      .borrow()
      .get_handle(mod_id)
      .map(|handle| v8::Local::new(scope, handle))
      .expect("Dyn import module info not found");
    // Resolution success
    assert_eq!(module.get_status(), v8::ModuleStatus::Evaluated);

    // IMPORTANT: No borrows to `ModuleMap` can be held at this point because
    // resolving the promise might initiate another `import()` which will
    // in turn call `bindings::host_import_module_dynamically_callback` which
    // will reach into `ModuleMap` from within the isolate.
    let module_namespace = module.get_module_namespace();
    resolver.resolve(scope, module_namespace).unwrap();
    self.dyn_module_evaluate_idle_counter.set(0);
    scope.perform_microtask_checkpoint();
  }

  /// Poll for progress in the module loading logic. Note that this takes a waker but
  /// doesn't act like a normal polling method.
  pub(crate) fn poll_progress(
    &self,
    cx: &mut Context,
    scope: &mut v8::HandleScope,
  ) -> Result<(), Error> {
    let mut has_evaluated = true;

    // TODO(mmastrac): We register this waker unconditionally because we occasionally need to re-run
    // the event loop. Eventually we will want this method to correctly wake the waker on any forward
    // progress.
    self.module_waker.register(cx.waker());

    // Run in a loop so that dynamic imports that only depend on another
    // dynamic import can be resolved in this event loop iteration.
    //
    // For example, a dynamically imported module like the following can be
    // immediately resolved after `dependency.ts` is fully evaluated, but it
    // wouldn't if not for this loop.
    //
    //    await delay(1000);
    //    await import("./dependency.ts");
    //    console.log("test")
    //
    // These dynamic import dependencies can be cross-realm:
    //
    //    await delay(1000);
    //    await new ShadowRealm().importValue("./dependency.js", "default");
    //
    while has_evaluated {
      has_evaluated = false;
      loop {
        let poll_imports = self.poll_prepare_dyn_imports(cx, scope)?;
        assert!(poll_imports.is_ready());

        let poll_imports = self.poll_dyn_imports(cx, scope)?;
        assert!(poll_imports.is_ready());

        if self.evaluate_dyn_imports(scope) {
          has_evaluated = true;
        } else {
          break;
        }
      }
    }

    Ok(())
  }

  fn poll_prepare_dyn_imports(
    &self,
    cx: &mut Context,
    scope: &mut v8::HandleScope,
  ) -> Poll<Result<(), Error>> {
    if !self.preparing_dynamic_imports_pending.get() {
      return Poll::Ready(Ok(()));
    }

    loop {
      let poll_result = self
        .preparing_dynamic_imports
        .borrow_mut()
        .poll_next_unpin(cx);

      if let Poll::Ready(Some(prepare_poll)) = poll_result {
        let dyn_import_id = prepare_poll.0;
        let prepare_result = prepare_poll.1;

        match prepare_result {
          Ok(load) => {
            self
              .pending_dynamic_imports
              .borrow_mut()
              .push(load.into_future());
            self.pending_dynamic_imports_pending.set(true);
          }
          Err(err) => {
            let exception = to_v8_type_error(scope, err);
            self.dynamic_import_reject(scope, dyn_import_id, exception);
          }
        }
        // Continue polling for more prepared dynamic imports.
        continue;
      }

      // There are no active dynamic import loads, or none are ready.
      self
        .preparing_dynamic_imports_pending
        .set(!self.preparing_dynamic_imports.borrow().is_empty());
      return Poll::Ready(Ok(()));
    }
  }

  fn poll_dyn_imports(
    &self,
    cx: &mut Context,
    scope: &mut v8::HandleScope,
  ) -> Poll<Result<(), Error>> {
    if !self.pending_dynamic_imports_pending.get() {
      return Poll::Ready(Ok(()));
    }

    loop {
      let poll_result = self
        .pending_dynamic_imports
        .borrow_mut()
        .poll_next_unpin(cx);

      if let Poll::Ready(Some(load_stream_poll)) = poll_result {
        let maybe_result = load_stream_poll.0;
        let mut load = load_stream_poll.1;
        let dyn_import_id = load.id;

        if let Some(load_stream_result) = maybe_result {
          match load_stream_result {
            Ok((request, info)) => {
              // A module (not necessarily the one dynamically imported) has been
              // fetched. Create and register it, and if successful, poll for the
              // next recursive-load event related to this dynamic import.
              let register_result =
                load.register_and_recurse(scope, &request, info);

              match register_result {
                Ok(()) => {
                  // Keep importing until it's fully drained
                  self
                    .pending_dynamic_imports
                    .borrow_mut()
                    .push(load.into_future());
                  self.pending_dynamic_imports_pending.set(true);
                }
                Err(err) => {
                  let exception = match err {
                    ModuleError::Exception(e) => e,
                    ModuleError::Other(e) => to_v8_type_error(scope, e),
                  };
                  self.dynamic_import_reject(scope, dyn_import_id, exception)
                }
              }
            }
            Err(err) => {
              // A non-javascript error occurred; this could be due to a an invalid
              // module specifier, or a problem with the source map, or a failure
              // to fetch the module source code.
              let exception = to_v8_type_error(scope, err);
              self.dynamic_import_reject(scope, dyn_import_id, exception);
            }
          }
        } else {
          // The top-level module from a dynamic import has been instantiated.
          // Load is done.
          let module_id =
            load.root_module_id.expect("Root module should be loaded");
          let result = self.instantiate_module(scope, module_id);
          if let Err(exception) = result {
            self.dynamic_import_reject(scope, dyn_import_id, exception);
          }
          self.dynamic_import_module_evaluate(
            scope,
            dyn_import_id,
            module_id,
          )?;
        }

        // Continue polling for more ready dynamic imports.
        continue;
      }

      // There are no active dynamic import loads, or none are ready.
      self
        .pending_dynamic_imports_pending
        .set(!self.pending_dynamic_imports.borrow().is_empty());
      return Poll::Ready(Ok(()));
    }
  }

  /// Returns the namespace object of a module.
  ///
  /// This is only available after module evaluation has completed.
  /// This function panics if module has not been instantiated.
  pub fn get_module_namespace(
    &self,
    scope: &mut v8::HandleScope,
    module_id: ModuleId,
  ) -> Result<v8::Global<v8::Object>, Error> {
    let module_handle = self
      .data
      .borrow()
      .get_handle(module_id)
      .expect("ModuleInfo not found");

    let module = module_handle.open(scope);

    if module.get_status() == v8::ModuleStatus::Errored {
      let exception = module.get_exception();
      return exception_to_err_result(scope, exception, false, false);
    }

    assert!(matches!(
      module.get_status(),
      v8::ModuleStatus::Instantiated | v8::ModuleStatus::Evaluated
    ));

    let module_namespace: v8::Local<v8::Object> =
      v8::Local::try_from(module.get_module_namespace())
        .map_err(|err: v8::DataError| generic_error(err.to_string()))?;

    Ok(v8::Global::new(scope, module_namespace))
  }

  /// Clear the module map, meant to be used after initializing extensions.
  /// Optionally pass a list of exceptions `(old_name, new_name)` representing
  /// specifiers which will be renamed and preserved in the module map.
  pub fn clear_module_map(&self, exceptions: &'static [&'static str]) {
    let handles = exceptions
      .iter()
      .map(|mod_name| (self.get_handle_by_name(mod_name).unwrap(), mod_name))
      .collect::<Vec<_>>();
    *self.data.borrow_mut() = ModuleMapData::default();
    for (handle, new_name) in handles {
      self.inject_handle(
        ModuleName::from_static(new_name),
        ModuleType::JavaScript,
        handle,
      )
    }
  }

  fn get_stalled_top_level_await_message_for_module(
    &self,
    scope: &mut v8::HandleScope,
    module_id: ModuleId,
  ) -> Vec<v8::Global<v8::Message>> {
    let data = self.data.borrow();
    let module_handle = data.handles.get(module_id).unwrap();

    let module = v8::Local::new(scope, module_handle);
    let stalled = module.get_stalled_top_level_await_message(scope);
    let mut messages = vec![];
    for (_, message) in stalled {
      messages.push(v8::Global::new(scope, message));
    }
    messages
  }

  pub(crate) fn find_stalled_top_level_await(
    &self,
    scope: &mut v8::HandleScope,
  ) -> Vec<v8::Global<v8::Message>> {
    // First check if that's root module
    let root_module_id = self
      .data
      .borrow()
      .info
      .iter()
      .filter(|m| m.main)
      .map(|m| m.id)
      .next();

    if let Some(root_module_id) = root_module_id {
      let messages = self
        .get_stalled_top_level_await_message_for_module(scope, root_module_id);
      if !messages.is_empty() {
        return messages;
      }
    }

    // It wasn't a top module, so iterate over all modules and try to find
    // any with stalled top level await
    for module_id in 0..self.data.borrow().handles.len() {
      let messages =
        self.get_stalled_top_level_await_message_for_module(scope, module_id);
      if !messages.is_empty() {
        return messages;
      }
    }

    vec![]
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
    scope: &mut v8::HandleScope,
    module_specifier: &str,
    source_code: ModuleCodeString,
  ) -> Result<v8::Global<v8::Value>, Error> {
    let specifier = ModuleSpecifier::parse(module_specifier)?;
    let mod_id = self
      .new_es_module(scope, false, specifier.into(), source_code, false)
      .map_err(|e| match e {
        ModuleError::Exception(exception) => {
          let exception = v8::Local::new(scope, exception);
          exception_to_err_result::<()>(scope, exception, false, true)
            .unwrap_err()
        }
        ModuleError::Other(error) => error,
      })?;

    self.instantiate_module(scope, mod_id).map_err(|e| {
      let exception = v8::Local::new(scope, e);
      exception_to_err_result::<()>(scope, exception, false, true).unwrap_err()
    })?;

    let module_handle = self.get_handle(mod_id).unwrap();
    let module_local = v8::Local::<v8::Module>::new(scope, module_handle);

    let status = module_local.get_status();
    assert_eq!(status, v8::ModuleStatus::Instantiated);

    let value = module_local.evaluate(scope).unwrap();
    let promise = v8::Local::<v8::Promise>::try_from(value).unwrap();
    let result = promise.result(scope);
    if !result.is_undefined() {
      return Err(
        exception_to_err_result::<()>(scope, result, false, true).unwrap_err(),
      );
    }

    let status = module_local.get_status();
    assert_eq!(status, v8::ModuleStatus::Evaluated);

    let mod_ns = module_local.get_module_namespace();

    Ok(v8::Global::new(scope, mod_ns))
  }

  pub(crate) fn add_lazy_loaded_esm_sources(
    &self,
    sources: &[ExtensionFileSource],
  ) {
    if sources.is_empty() {
      return;
    }

    let data = self.data.borrow_mut();
    data.lazy_esm_sources.borrow_mut().extend(
      sources
        .iter()
        .cloned()
        .map(|source| (source.specifier, source)),
    );
  }

  /// Lazy load and evaluate an ES module. Only modules that have been added
  /// during build time can be executed (the ones stored in
  /// `ModuleMapData::lazy_esm_sources`), not _any, random_ module.
  pub(crate) fn lazy_load_esm_module(
    &self,
    scope: &mut v8::HandleScope,
    module_specifier: &str,
  ) -> Result<v8::Global<v8::Value>, Error> {
    let lazy_esm_sources = self.data.borrow().lazy_esm_sources.clone();
    let loader = LazyEsmModuleLoader::new(lazy_esm_sources);

    // Check if this module has already been loaded.
    {
      let module_map_data = self.data.borrow();
      if let Some(id) =
        module_map_data.get_id(module_specifier, RequestedModuleType::None)
      {
        let handle = module_map_data.get_handle(id).unwrap();
        let handle_local = v8::Local::new(scope, handle);
        let module =
          v8::Global::new(scope, handle_local.get_module_namespace());
        return Ok(module);
      }
    }

    let specifier = ModuleSpecifier::parse(module_specifier)?;
    let source = futures::executor::block_on(async {
      loader.load(&specifier, None, false).await
    })?;

    self.lazy_load_es_module_from_code(
      scope,
      module_specifier,
      ModuleSource::get_string_source(specifier.as_str(), source.code)?,
    )
  }
}

// Clippy thinks the return value doesn't need to be an Option, it's unaware
// of the mapping that MapFnFrom<F> does for ResolveModuleCallback.
#[allow(clippy::unnecessary_wraps)]
fn synthetic_module_evaluation_steps<'a>(
  context: v8::Local<'a, v8::Context>,
  module: v8::Local<v8::Module>,
) -> Option<v8::Local<'a, v8::Value>> {
  // SAFETY: `CallbackScope` can be safely constructed from `Local<Context>`
  let scope = &mut unsafe { v8::CallbackScope::new(context) };
  let tc_scope = &mut v8::TryCatch::new(scope);
  let module_map = JsRealm::module_map_from(tc_scope);

  let handle = v8::Global::<v8::Module>::new(tc_scope, module);
  let value_handle = module_map
    .data
    .borrow_mut()
    .synthetic_module_value_store
    .remove(&handle)
    .unwrap();
  let value_local = v8::Local::new(tc_scope, value_handle);

  let name = v8::String::new(tc_scope, "default").unwrap();
  // This should never fail
  assert!(
    module.set_synthetic_module_export(tc_scope, name, value_local)
      == Some(true)
  );
  assert!(!tc_scope.has_caught());

  // Since TLA is active we need to return a promise.
  let resolver = v8::PromiseResolver::new(tc_scope).unwrap();
  let undefined = v8::undefined(tc_scope);
  resolver.resolve(tc_scope, undefined.into());
  Some(resolver.get_promise(tc_scope).into())
}

pub fn module_origin<'a>(
  s: &mut v8::HandleScope<'a>,
  resource_name: v8::Local<'a, v8::String>,
) -> v8::ScriptOrigin<'a> {
  let source_map_url = v8::String::empty(s);
  v8::ScriptOrigin::new(
    s,
    resource_name.into(),
    0,
    0,
    false,
    123,
    source_map_url.into(),
    true,
    false,
    true,
  )
}
