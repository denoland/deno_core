// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
use super::module_map_data::ModuleMapSnapshotData;
use super::IntoModuleCodeString;
use super::IntoModuleName;
use crate::ascii_str;
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
use crate::runtime::SnapshotLoadDataStore;
use crate::runtime::SnapshotStoreDataStore;
use crate::FastStaticString;
use crate::JsRuntime;
use crate::ModuleLoadResponse;
use crate::ModuleSource;
use crate::ModuleSpecifier;
use anyhow::bail;
use anyhow::Context as _;
use anyhow::Error;
use futures::future::FutureExt;
use futures::stream::FuturesUnordered;
use futures::stream::StreamFuture;
use futures::task::noop_waker_ref;
use futures::task::AtomicWaker;
use futures::Future;
use futures::StreamExt;
use v8::Function;
use v8::PromiseState;

use std::borrow::Cow;
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
use super::CustomModuleEvaluationKind;
use super::LazyEsmModuleLoader;
use super::RequestedModuleType;

type PrepareLoadFuture =
  dyn Future<Output = (ModuleLoadId, Result<RecursiveModuleLoad, Error>)>;

type CodeCacheReadyFuture = dyn Future<Output = ()>;

use super::ImportMetaResolveCallback;

struct ModEvaluate {
  module_map: Rc<ModuleMap>,
  sender: Option<oneshot::Sender<Result<(), Error>>>,
  module: Option<v8::Global<v8::Module>>,
  notify: Vec<v8::Global<v8::Function>>,
}

impl ModEvaluate {
  fn notify(&mut self, scope: &mut v8::HandleScope) {
    if !self.notify.is_empty() {
      let module = v8::Local::new(scope, self.module.take().unwrap());
      let ns = module.get_module_namespace();
      let recv = v8::undefined(scope).into();
      let args = &[ns];
      for notify in std::mem::take(&mut self.notify).into_iter() {
        let notify = v8::Local::new(scope, notify);
        notify.call(scope, recv, args);
      }
    }
    _ = self.sender.take().unwrap().send(Ok(()));
  }
}

type CodeCacheReadyCallback =
  Box<dyn FnOnce(&[u8]) -> Pin<Box<dyn Future<Output = ()>>>>;
pub(crate) struct CodeCacheInfo {
  data: Option<Cow<'static, [u8]>>,
  ready_callback: CodeCacheReadyCallback,
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
  code_cache_ready_futs:
    RefCell<FuturesUnordered<Pin<Box<CodeCacheReadyFuture>>>>,
  pending_code_cache_ready: Cell<bool>,
  module_waker: AtomicWaker,
  data: RefCell<ModuleMapData>,

  /// A counter used to delay our dynamic import deadlock detection by one spin
  /// of the event loop.
  pub(crate) dyn_module_evaluate_idle_counter: Cell<u32>,
}

impl ModuleMap {
  /// There is a circular Rc reference between the module map and the futures,
  /// so when destroying the module map we need to clear the pending futures.
  pub(crate) fn destroy(&self) {
    self.dynamic_import_map.borrow_mut().clear();
    self.preparing_dynamic_imports.borrow_mut().clear();
    self.pending_dynamic_imports.borrow_mut().clear();
    self.code_cache_ready_futs.borrow_mut().clear();
    std::mem::take(&mut *self.data.borrow_mut());
  }

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
      code_cache_ready_futs: Default::default(),
      pending_code_cache_ready: Default::default(),
      module_waker: Default::default(),
      data: Default::default(),
    }
  }

  pub(crate) fn update_with_snapshotted_data(
    &self,
    scope: &mut v8::HandleScope,
    data_store: &mut SnapshotLoadDataStore,
    data: ModuleMapSnapshotData,
  ) {
    self
      .data
      .borrow_mut()
      .update_with_snapshotted_data(scope, data_store, data);
  }

  /// Get module id, following all aliases in case of module specifier
  /// that had been redirected.
  pub(crate) fn get_id(
    &self,
    name: &str,
    requested_module_type: impl AsRef<RequestedModuleType>,
  ) -> Option<ModuleId> {
    self.data.borrow().get_id(name, requested_module_type)
  }

  pub(crate) fn is_main_module(&self, global: &v8::Global<v8::Module>) -> bool {
    self.data.borrow().is_main_module(global)
  }

  pub(crate) fn is_main_module_id(&self, id: ModuleId) -> bool {
    self.data.borrow().main_module_id == Some(id)
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

  pub(crate) fn get_type_by_module(
    &self,
    global: &v8::Global<v8::Module>,
  ) -> Option<ModuleType> {
    self.data.borrow().get_type_by_module(global)
  }

  pub(crate) fn get_handle(
    &self,
    id: ModuleId,
  ) -> Option<v8::Global<v8::Module>> {
    self.data.borrow().get_handle(id)
  }

  pub(crate) fn serialize_for_snapshotting(
    &self,
    data_store: &mut SnapshotStoreDataStore,
  ) -> ModuleMapSnapshotData {
    let data = std::mem::take(&mut *self.data.borrow_mut());
    data.serialize_for_snapshotting(data_store)
  }

  #[cfg(test)]
  pub fn is_alias(
    &self,
    name: &str,
    requested_module_type: impl AsRef<RequestedModuleType>,
  ) -> bool {
    self.data.borrow().is_alias(name, requested_module_type)
  }

  pub(crate) fn get_data(&self) -> &RefCell<ModuleMapData> {
    &self.data
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
      code_cache,
    } = module_source;

    // Register the module in the module map unless it's already there. If the
    // specified URL and the "true" URL are different, register the alias.
    let module_url_found = if let Some(module_url_found) = module_url_found {
      let (module_url_found1, module_url_found2) =
        module_url_found.into_cheap_copy();
      self.data.borrow_mut().alias(
        module_url_specified,
        &module_type.clone().into(),
        module_url_found1,
      );
      module_url_found2
    } else {
      module_url_specified
    };

    // TODO(bartlomieju): I have a hunch that this is wrong - write a test
    // that tries to "confuse" the type system, by first requesting a module
    // with type `RequestedModuleType::Other("foo".into)``, and then the loader
    // actually returns `ModuleType::Other("bar".into())`. See if it leads to
    // unexpected result in how `ModuleMap` is structured and verify how
    // querying the module map works (`ModuleMap::get_by_id`, `ModuleMap::get_by_name`).
    let requested_module_type = RequestedModuleType::from(module_type.clone());
    let maybe_module_id = self.get_id(&module_url_found, requested_module_type);

    if let Some(module_id) = maybe_module_id {
      return Ok(module_id);
    }

    let module_id = match module_type {
      ModuleType::JavaScript => {
        let code = ModuleSource::get_string_source(code);

        let (code_cache_info, module_url_found) =
          if let Some(code_cache) = code_cache {
            let (module_url_found1, module_url_found2) =
              module_url_found.into_cheap_copy();
            let loader = self.loader.borrow().clone();
            (
              Some(CodeCacheInfo {
                data: code_cache.data,
                ready_callback: Box::new(move |cache| {
                  let specifier =
                    ModuleSpecifier::parse(module_url_found1.as_str()).unwrap();
                  loader.code_cache_ready(specifier, code_cache.hash, cache)
                }),
              }),
              module_url_found2,
            )
          } else {
            (None, module_url_found)
          };

        self.new_module_from_js_source(
          scope,
          main,
          ModuleType::JavaScript,
          module_url_found,
          code,
          dynamic,
          code_cache_info,
        )?
      }
      ModuleType::Wasm => {
        return Err(ModuleError::Other(generic_error(
          "Importing Wasm modules is currently not supported.",
        )));
      }
      ModuleType::Json => {
        let code = ModuleSource::get_string_source(code);
        self.new_json_module(scope, module_url_found, code)?
      }
      ModuleType::Other(module_type) => {
        let state = JsRuntime::state_from(scope);
        let custom_module_evaluation_cb =
          state.custom_module_evaluation_cb.as_ref();

        let Some(custom_evaluation_cb) = custom_module_evaluation_cb else {
          return Err(ModuleError::Other(generic_error(format!(
            "Importing '{}' modules is not supported",
            module_type
          ))));
        };

        // TODO(bartlomieju): creating a global just to create a local from it
        // seems superfluous. However, changing `CustomModuleEvaluationCb` to have
        // a lifetime will have a viral effect and required `JsRuntimeOptions`
        // to have a callback as well as `JsRuntime`.
        let module_evaluation_kind = custom_evaluation_cb(
          scope,
          module_type.clone(),
          &module_url_found,
          code,
        )
        .map_err(ModuleError::Other)?;

        match module_evaluation_kind {
          // Simple case, we just got a single value so we create a regular
          // synthetic module.
          CustomModuleEvaluationKind::Synthetic(value_global) => {
            let value = v8::Local::new(scope, value_global);
            let exports = vec![(ascii_str!("default"), value)];
            self.new_synthetic_module(
              scope,
              module_url_found,
              ModuleType::Other(module_type.clone()),
              exports,
            )?
          }

          // Complex case - besides a synthetic module, we will create a new
          // module from JS code.
          CustomModuleEvaluationKind::ComputedAndSynthetic(
            computed_src,
            synthetic_value,
            synthetic_module_type,
          ) => {
            let (url1, url2) = module_url_found.into_cheap_copy();
            let value = v8::Local::new(scope, synthetic_value);
            let exports = vec![(ascii_str!("default"), value)];
            let _synthetic_mod_id = self.new_synthetic_module(
              scope,
              url1,
              synthetic_module_type,
              exports,
            )?;

            let (code_cache_info, url2) = if let Some(code_cache) = code_cache {
              let (url1, url2) = url2.into_cheap_copy();
              let loader = self.loader.borrow().clone();
              (
                Some(CodeCacheInfo {
                  data: code_cache.data,
                  ready_callback: Box::new(move |cache| {
                    let specifier =
                      ModuleSpecifier::parse(url1.as_str()).unwrap();
                    loader.code_cache_ready(specifier, code_cache.hash, cache)
                  }),
                }),
                url2,
              )
            } else {
              (None, url2)
            };

            self.new_module_from_js_source(
              scope,
              main,
              ModuleType::Other(module_type.clone()),
              url2,
              computed_src,
              dynamic,
              code_cache_info,
            )?
          }
        }
      }
    };
    Ok(module_id)
  }

  /// Creates a "synthetic module", that contains only a single, "default" export.
  ///
  /// The module gets instantiated and its ID is returned.
  pub fn new_synthetic_module(
    &self,
    scope: &mut v8::HandleScope,
    name: impl IntoModuleName,
    module_type: ModuleType,
    exports: Vec<(FastStaticString, v8::Local<v8::Value>)>,
  ) -> Result<ModuleId, ModuleError> {
    let name = name.into_module_name();
    let name_str = name.v8_string(scope);

    let export_names = exports
      .iter()
      .map(|(name, _)| name.v8_string(scope))
      .collect::<Vec<_>>();
    let module = v8::Module::create_synthetic_module(
      scope,
      name_str,
      &export_names,
      synthetic_module_evaluation_steps,
    );

    let handle = v8::Global::<v8::Module>::new(scope, module);
    let mut exports_global = Vec::with_capacity(exports.len());

    for i in 0..exports.len() {
      let export_name = export_names[i];
      let (_, export_value) = exports[i];
      exports_global.push((
        v8::Global::new(scope, export_name),
        v8::Global::new(scope, export_value),
      ));
    }

    self
      .data
      .borrow_mut()
      .synthetic_module_exports_store
      .insert(handle.clone(), exports_global);

    let id = self.data.borrow_mut().create_module_info(
      name,
      module_type,
      handle,
      false,
      vec![],
    );

    // Synthetic modules have no imports so their instantation must never fail.
    self.instantiate_module(scope, id).unwrap();

    Ok(id)
  }

  // TODO(bartlomieju): remove this method or rename it to `new_js_module`.
  /// Create and compile an ES module.
  pub(crate) fn new_es_module(
    &self,
    scope: &mut v8::HandleScope,
    main: bool,
    name: impl IntoModuleName,
    source: impl IntoModuleCodeString,
    is_dynamic_import: bool,
    code_cache_info: Option<CodeCacheInfo>,
  ) -> Result<ModuleId, ModuleError> {
    let name = name.into_module_name();
    self.new_module_from_js_source(
      scope,
      main,
      ModuleType::JavaScript,
      name.into_module_name(),
      source.into_module_code(),
      is_dynamic_import,
      code_cache_info,
    )
  }

  /// Provided given JavaScript source code, compile and create a module of given
  /// type.
  ///
  /// Passed type doesn't have to be [`ModuleType::JavaScript`]! This method
  /// can be used to create "shim" modules, that execute some JS and act as a
  /// proxy to the actual underlying module (eg. you might create a "shim" for
  /// WASM module).
  ///
  /// Imports in the executed code are parsed (along their import attributes)
  /// and attached to associated [`ModuleInfo`].
  ///
  /// Returns an ID of newly created module.
  #[allow(clippy::too_many_arguments)]
  pub(crate) fn new_module_from_js_source(
    &self,
    scope: &mut v8::HandleScope,
    main: bool,
    module_type: ModuleType,
    name: ModuleName,
    source: ModuleCodeString,
    is_dynamic_import: bool,
    mut code_cache_info: Option<CodeCacheInfo>,
  ) -> Result<ModuleId, ModuleError> {
    if main {
      let data = self.data.borrow();
      if let Some(main_module) = data.main_module_id {
        let main_name = self.data.borrow().get_name_by_id(main_module).unwrap();
        return Err(ModuleError::Other(generic_error(
          format!("Trying to create \"main\" module ({:?}), when one already exists ({:?})",
          name,
          main_name,
        ))));
      }
    }

    let name_str = name.v8_string(scope);
    let source_str = source.v8_string(scope);

    let origin = module_origin(scope, name_str);

    let tc_scope = &mut v8::TryCatch::new(scope);

    let (maybe_module, try_store_code_cache) = code_cache_info
      .as_ref()
      .and_then(|code_cache_info| {
        code_cache_info.data.as_ref().map(|cache| {
          let mut source = v8::script_compiler::Source::new_with_cached_data(
            source_str,
            Some(&origin),
            v8::CachedData::new(cache),
          );
          let maybe_module = v8::script_compiler::compile_module2(
            tc_scope,
            &mut source,
            v8::script_compiler::CompileOptions::ConsumeCodeCache,
            v8::script_compiler::NoCacheReason::NoReason,
          );
          // Check if the provided code cache is rejected by V8.
          let rejected = match source.get_cached_data() {
            Some(cached_data) => cached_data.rejected(),
            _ => true,
          };
          (maybe_module, rejected)
        })
      })
      .unwrap_or_else(|| {
        let source =
          v8::script_compiler::Source::new(source_str, Some(&origin));
        (v8::script_compiler::compile_module(tc_scope, source), true)
      });

    if tc_scope.has_caught() {
      assert!(maybe_module.is_none());
      let exception = tc_scope.exception().unwrap();
      let exception = v8::Global::new(tc_scope, exception);
      // TODO(bartlomieju): add a more concrete variant - like `ModuleError::CompileError`?
      return Err(ModuleError::Exception(exception));
    }

    let module = maybe_module.unwrap();

    if try_store_code_cache {
      if let Some(code_cache_info) = code_cache_info.take() {
        let unbound_module_script = module.get_unbound_module_script(tc_scope);
        let code_cache =
          unbound_module_script.create_code_cache().ok_or_else(|| {
            ModuleError::Other(generic_error(
              "Unable to get code cache from unbound module script",
            ))
          })?;
        let fut =
          async move { (code_cache_info.ready_callback)(&code_cache).await }
            .boxed_local();
        self.code_cache_ready_futs.borrow_mut().push(fut);
        self.pending_code_cache_ready.set(true);
      }
    }

    // TODO(bartlomieju): maybe move to a helper function?
    let module_requests = module.get_module_requests();
    let requests_len = module_requests.length();
    let mut requests = Vec::with_capacity(requests_len);
    for i in 0..module_requests.length() {
      let module_request = v8::Local::<v8::ModuleRequest>::try_from(
        module_requests.get(tc_scope, i).unwrap(),
      )
      .unwrap();
      let import_specifier = module_request
        .get_specifier()
        .to_rust_string_lossy(tc_scope);

      let import_attributes = module_request.get_import_attributes();

      let attributes = parse_import_attributes(
        tc_scope,
        import_attributes,
        ImportAttributesKind::StaticImport,
      );

      // FIXME(bartomieju): there are no stack frames if exception
      // is thrown here
      {
        let state = JsRuntime::state_from(tc_scope);
        if let Some(validate_import_attributes_cb) =
          &state.validate_import_attributes_cb
        {
          (validate_import_attributes_cb)(tc_scope, &attributes);
        }
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
        specifier: module_specifier,
        requested_module_type,
      };
      requests.push(request);
    }

    let handle = v8::Global::<v8::Module>::new(tc_scope, module);
    let id = self.data.borrow_mut().create_module_info(
      name,
      module_type,
      handle,
      main,
      requests,
    );

    Ok(id)
  }

  pub(crate) fn new_json_module(
    &self,
    scope: &mut v8::HandleScope,
    name: impl IntoModuleName,
    code: impl IntoModuleCodeString,
  ) -> Result<ModuleId, ModuleError> {
    let name = name.into_module_name();
    let code = code.into_module_code();
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
    let exports = vec![(ascii_str!("default"), parsed_json)];
    self.new_synthetic_module(tc_scope, name, ModuleType::Json, exports)
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
      && !referrer.starts_with("checkin:")
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
          .is_registered(module_specifier.as_str(), requested_module_type)
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

      // If this is a main module, claim the main module notification functions
      let (notify, module) = if self.is_main_module_id(id) {
        let module = Some(v8::Global::new(tc_scope, module));
        (
          std::mem::take(&mut self.data.borrow_mut().main_module_callbacks),
          module,
        )
      } else {
        (vec![], None)
      };

      // Create a ModEvaluate instance and stash it in an external
      let evaluation = v8::External::new(
        tc_scope,
        Box::into_raw(Box::new(ModEvaluate {
          module_map: self.clone(),
          sender: Some(sender),
          notify,
          module,
        })) as _,
      );

      fn get_sender(arg: v8::Local<v8::Value>) -> ModEvaluate {
        let sender = v8::Local::<v8::External>::try_from(arg).unwrap();
        *unsafe { Box::from_raw(sender.value() as _) }
      }

      let on_fulfilled = Function::builder(
        |scope: &mut v8::HandleScope<'_>,
         args: v8::FunctionCallbackArguments<'_>,
         _rv: v8::ReturnValue| {
          let mut sender = get_sender(args.data());
          sender.module_map.pending_mod_evaluation.set(false);
          sender.module_map.module_waker.wake();
          sender.notify(scope);
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
        // There are two reasons we could be here:
        // 1. The runtime is shutting down, and JS ops are disabled with termination exceptions.
        // 2. User code has tampered with the runtime globals in some way that prevents us from
        //    attaching `on_fulfilled`/`on_rejected` to `promise`.
        // In these cases we still need to report something back, so synthesize the result from the
        // promise.

        // Unset pending mod evaluation as the handlers will never run. See debug_assert below.
        self.pending_mod_evaluation.set(false);

        let mut sender = get_sender(evaluation.into());
        match promise.state() {
          PromiseState::Fulfilled => {
            if let Some(exception) = tc_scope.exception() {
              _ = sender.sender.take().unwrap().send(exception_to_err_result(
                tc_scope, exception, true, false,
              ));
            } else {
              // Module loaded OK
              sender.notify(tc_scope);
            }
          }
          PromiseState::Rejected => {
            // Module was rejected
            let err = promise.result(tc_scope);
            let err = JsError::from_v8_exception(tc_scope, err);
            _ = sender.sender.take().unwrap().send(Err(err.into()));
          }
          PromiseState::Pending => {
            // User code shouldn't be able to both cause the runtime to fail and leave the promise as
            // pending because the only way to adopt a pending promise is to use `await` and
            // `await` won't work if you've broken the runtime in such a way that `promise::then`
            // didn't work.
            debug_assert!(tc_scope.is_execution_terminating());
            // Module pending, just drop the sender at this point -- we can't do anything with a shut-down runtime.
            drop(sender);
          }
        }
      }

      tc_scope.perform_microtask_checkpoint();
    }

    receiver
  }

  /// Helper function that allows to evaluate a module and ensure it's fully
  /// evaluated without the need to poll the event loop.
  ///
  /// This is useful for evaluating internal modules that can't use Top-Level Await.
  pub(crate) fn mod_evaluate_sync(
    self: &Rc<Self>,
    scope: &mut v8::HandleScope,
    id: ModuleId,
  ) -> Result<(), Error> {
    let mut receiver = self.mod_evaluate(scope, id);

    // After evaluate_pending_module, if the module isn't fully evaluated
    // and the resolver solved, it means the module or one of its imports
    // uses TLA.
    match receiver.poll_unpin(&mut Context::from_waker(noop_waker_ref())) {
      Poll::Ready(result) => {
        result.with_context(|| {
          let specifier = self.get_name_by_id(id).unwrap();
          format!("Couldn't execute '{specifier}'")
        })?;
      }
      Poll::Pending => {
        // Find the TLA location and return it as an error
        let messages = self.find_stalled_top_level_await(scope);
        assert!(!messages.is_empty());
        let msg = v8::Local::new(scope, &messages[0]);
        let js_error = JsError::from_v8_message(scope, msg);
        return Err(Error::from(js_error))
          .with_context(|| "Top-level await is not allowed in extensions");
      }
    }

    Ok(())
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
    // the user. We add handlers to wake the event loop when the promise resolves
    // (or rejects). The catch handler also serves to prevent an exception if the internal promise
    // rejects. That will instead happen for the other if not handled by the user.
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

      fn wake_module(
        scope: &mut v8::HandleScope<'_>,
        _args: v8::FunctionCallbackArguments<'_>,
        _rv: v8::ReturnValue,
      ) {
        let module_map = JsRealm::module_map_from(scope);
        module_map.module_waker.wake();
      }

      let promise = v8::Local::<v8::Promise>::try_from(value)
        .expect("Expected to get promise as module evaluation result");

      let wake_module_cb = Function::builder(wake_module).build(tc_scope);

      if let Some(wake_module_cb) = wake_module_cb {
        promise.then2(tc_scope, wake_module_cb, wake_module_cb);
      } else {
        // If the runtime is shutting down, we can't attach the handlers.
        // It doesn't really matter though, because they're just for waking the
        // event loop.
      }
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

        let poll_code_cache_ready = self.poll_code_cache_ready(cx)?;
        assert!(poll_code_cache_ready.is_ready());

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

  fn poll_code_cache_ready(&self, cx: &mut Context) -> Poll<Result<(), Error>> {
    if !self.pending_code_cache_ready.get() {
      return Poll::Ready(Ok(()));
    }

    loop {
      let poll_result =
        self.code_cache_ready_futs.borrow_mut().poll_next_unpin(cx);

      if let Poll::Ready(Some(_)) = poll_result {
        continue;
      }

      self
        .pending_code_cache_ready
        .set(!self.code_cache_ready_futs.borrow().is_empty());
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

  fn get_stalled_top_level_await_message_for_module(
    &self,
    scope: &mut v8::HandleScope,
    module_id: ModuleId,
  ) -> Vec<v8::Global<v8::Message>> {
    let data = self.data.borrow();
    let module_handle = data.handles.get(module_id).unwrap();

    let module = v8::Local::new(scope, module_handle);
    // v8::Module::GetStalledTopLevelAwaitMessage() must not be called on
    // a synthetic module.
    if module.is_synthetic_module() {
      return vec![];
    }

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
  pub(crate) fn lazy_load_es_module_with_code(
    &self,
    scope: &mut v8::HandleScope,
    module_specifier: &str,
    source_code: ModuleCodeString,
    code_cache_info: Option<CodeCacheInfo>,
  ) -> Result<v8::Global<v8::Value>, Error> {
    let specifier = ModuleSpecifier::parse(module_specifier)?;
    let mod_id = self
      .new_es_module(
        scope,
        false,
        specifier.clone(),
        source_code,
        false,
        code_cache_info,
      )
      .map_err(|e| e.into_any_error(scope, false, true))?;

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

  pub(crate) fn add_lazy_loaded_esm_source(
    &self,
    specifier: ModuleName,
    code: ModuleCodeString,
  ) {
    let data = self.data.borrow_mut();
    assert!(data
      .lazy_esm_sources
      .borrow_mut()
      .insert(specifier, code)
      .is_none());
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

    let load_response =
      loader.load(&specifier, None, false, RequestedModuleType::None);

    let source = match load_response {
      ModuleLoadResponse::Sync(result) => result,
      ModuleLoadResponse::Async(fut) => futures::executor::block_on(fut),
    }?;

    self.lazy_load_es_module_with_code(
      scope,
      module_specifier,
      ModuleSource::get_string_source(source.code),
      if let Some(code_cache) = source.code_cache {
        let loader = self.loader.borrow().clone();
        Some(CodeCacheInfo {
          data: code_cache.data,
          ready_callback: Box::new(move |cache| {
            loader.code_cache_ready(specifier, code_cache.hash, cache)
          }),
        })
      } else {
        None
      },
    )
  }
}

// Clippy thinks the return value doesn't need to be an Option, it's unaware
// of the mapping that MapFnFrom<F> does for ResolveModuleCallback.
#[allow(clippy::unnecessary_wraps)]
pub(crate) fn synthetic_module_evaluation_steps<'a>(
  context: v8::Local<'a, v8::Context>,
  module: v8::Local<v8::Module>,
) -> Option<v8::Local<'a, v8::Value>> {
  // SAFETY: `CallbackScope` can be safely constructed from `Local<Context>`
  let scope = &mut unsafe { v8::CallbackScope::new(context) };
  let tc_scope = &mut v8::TryCatch::new(scope);
  let module_map = JsRealm::module_map_from(tc_scope);

  let handle = v8::Global::<v8::Module>::new(tc_scope, module);
  let exports = module_map
    .data
    .borrow_mut()
    .synthetic_module_exports_store
    .remove(&handle)
    .unwrap();

  for (export_name, export_value) in exports {
    let name = v8::Local::new(tc_scope, export_name);
    let value = v8::Local::new(tc_scope, export_value);

    // This should never fail
    assert!(module
      .set_synthetic_module_export(tc_scope, name, value)
      .unwrap());
    assert!(!tc_scope.has_caught());
  }

  // Since Top-Level Await is active we need to return a promise.
  // This promise is resolved immediately.
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
