// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use crate::error::exception_to_err_result;
use crate::error::generic_error;
use crate::error::throw_type_error;
use crate::error::to_v8_type_error;
use crate::fast_string::FastString;
use crate::modules::get_asserted_module_type_from_assertions;
use crate::modules::parse_import_assertions;
use crate::modules::ImportAssertionsKind;
use crate::modules::ModuleCode;
use crate::modules::ModuleError;
use crate::modules::ModuleId;
use crate::modules::ModuleInfo;
use crate::modules::ModuleLoadId;
use crate::modules::ModuleLoader;
use crate::modules::ModuleName;
use crate::modules::ModuleRequest;
use crate::modules::ModuleType;
use crate::modules::NoopModuleLoader;
use crate::modules::PrepareLoadFuture;
use crate::modules::RecursiveModuleLoad;
use crate::modules::ResolutionKind;
use crate::runtime::JsRealm;
use crate::runtime::SnapshottedData;
use crate::JsRuntime;
use anyhow::Error;
use futures::future::FutureExt;
use futures::stream::FuturesUnordered;
use futures::stream::StreamFuture;
use futures::StreamExt;
use std::cell::Cell;
use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::DerefMut;
use std::pin::Pin;
use std::rc::Rc;
use std::task::Context;
use std::task::Poll;

use super::AssertedModuleType;

pub const BOM_CHAR: &[u8] = &[0xef, 0xbb, 0xbf];

/// Strips the byte order mark from the provided text if it exists.
fn strip_bom(source_code: &[u8]) -> &[u8] {
  if source_code.starts_with(BOM_CHAR) {
    &source_code[BOM_CHAR.len()..]
  } else {
    source_code
  }
}

/// A symbolic module entity.
#[derive(Debug, PartialEq)]
pub(crate) enum SymbolicModule {
  /// This module is an alias to another module.
  /// This is useful such that multiple names could point to
  /// the same underlying module (particularly due to redirects).
  Alias(ModuleName),
  /// This module associates with a V8 module by id.
  Mod(ModuleId),
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

  dynamic_import_map:
    RefCell<HashMap<ModuleLoadId, v8::Global<v8::PromiseResolver>>>,
  preparing_dynamic_imports:
    RefCell<FuturesUnordered<Pin<Box<PrepareLoadFuture>>>>,
  pending_dynamic_imports:
    RefCell<FuturesUnordered<StreamFuture<RecursiveModuleLoad>>>,
  pending_dyn_mod_evaluations: RefCell<Vec<DynImportModEvaluate>>,
  data: RefCell<ModuleMapData>,

  /// A counter used to delay our dynamic import deadlock detection by one spin
  /// of the event loop.
  pub(crate) dyn_module_evaluate_idle_counter: Cell<u32>,
}

/// Map of [`ModuleName`] and [`AssertedModuleType`] to a data field.
struct ModuleNameTypeMap<T> {
  submaps: Vec<HashMap<ModuleName, T>>,
  map_index: HashMap<AssertedModuleType, usize>,
  len: usize,
}

impl<T> Default for ModuleNameTypeMap<T> {
  fn default() -> Self {
    Self {
      submaps: Default::default(),
      map_index: Default::default(),
      len: 0,
    }
  }
}

impl<T> ModuleNameTypeMap<T> {
  pub fn len(&self) -> usize {
    self.len
  }

  fn map_index(&self, ty: &AssertedModuleType) -> Option<usize> {
    self.map_index.get(ty).copied()
  }

  pub fn get<Q>(&self, ty: &AssertedModuleType, name: &Q) -> Option<&T>
  where
    ModuleName: std::borrow::Borrow<Q>,
    Q: std::cmp::Eq + std::hash::Hash + ?Sized,
  {
    let Some(index) = self.map_index(ty) else {
      return None;
    };
    let Some(map) = self.submaps.get(index) else {
      return None;
    };
    map.get(name)
  }

  pub fn clear(&mut self) {
    self.len = 0;
    self.map_index.clear();
    self.submaps.clear();
  }

  pub fn insert(
    &mut self,
    module_type: &AssertedModuleType,
    name: FastString,
    module: T,
  ) {
    let index = match self.map_index(module_type) {
      Some(index) => index,
      None => {
        let index = self.submaps.len();
        self.map_index.insert(module_type.clone(), index);
        self.submaps.push(Default::default());
        index
      }
    };

    if self
      .submaps
      .get_mut(index)
      .unwrap()
      .insert(name, module)
      .is_none()
    {
      self.len += 1;
    }
  }

  /// Rather than providing an iterator, we provide a walk method. This is mainly because Rust
  /// doesn't have generators.
  pub fn walk(
    &self,
    mut f: impl FnMut(usize, &AssertedModuleType, &ModuleName, &T),
  ) {
    let mut i = 0;
    for (ty, value) in self.map_index.iter() {
      for (key, value) in self.submaps.get(*value).unwrap() {
        f(i, ty, key, value);
        i += 1;
      }
    }
  }
}

#[derive(Default)]
pub(crate) struct ModuleMapData {
  /// Inverted index from module to index in `info`.
  handles_inverted: HashMap<v8::Global<v8::Module>, usize>,
  /// The handles we have loaded so far, corresponding with the [`ModuleInfo`] in `info`.
  handles: Vec<v8::Global<v8::Module>>,
  /// The modules we have loaded so far.
  info: Vec<ModuleInfo>,
  /// [`ModuleName`] to [`SymbolicModule`] for modules.
  by_name: ModuleNameTypeMap<SymbolicModule>,
  /// The next ID used for a load.
  next_load_id: ModuleLoadId,
  /// If a main module has been loaded, points to it by index.
  main_module_id: Option<ModuleId>,
  /// This store is used temporarily, to forward parsed JSON
  /// value from `new_json_module` to `json_module_evaluation_steps`
  json_value_store: HashMap<v8::Global<v8::Module>, v8::Global<v8::Value>>,
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
  pub(crate) fn assert_all_modules_evaluated(
    &self,
    scope: &mut v8::HandleScope,
  ) {
    let mut not_evaluated = vec![];
    let data = self.data.borrow();

    for (handle, i) in data.handles_inverted.iter() {
      let module = v8::Local::new(scope, handle);
      if !matches!(module.get_status(), v8::ModuleStatus::Evaluated) {
        not_evaluated.push(data.info[*i].name.as_str().to_string());
      }
    }

    if !not_evaluated.is_empty() {
      let mut msg = "Following modules were not evaluated; make sure they are imported from other code:\n".to_string();
      for m in not_evaluated {
        msg.push_str(&format!("  - {}\n", m));
      }
      panic!("{}", msg);
    }
  }

  pub fn serialize_for_snapshotting(
    &self,
    scope: &mut v8::HandleScope,
  ) -> SnapshottedData {
    let array = v8::Array::new(scope, 3);
    let data = self.data.borrow();

    let next_load_id = v8::Integer::new(scope, data.next_load_id);
    array.set_index(scope, 0, next_load_id.into());

    if let Some(main_module_id) = data.main_module_id {
      let main_module_id = v8::Integer::new(scope, main_module_id as i32);
      array.set_index(scope, 1, main_module_id.into());
    }

    let info_arr = v8::Array::new(scope, data.info.len() as i32);
    for (i, info) in data.info.iter().enumerate() {
      let module_info_arr = v8::Array::new(scope, 4);

      let id = v8::Integer::new(scope, info.id as i32);
      module_info_arr.set_index(scope, 0, id.into());

      let name = info.name.v8(scope);
      module_info_arr.set_index(scope, 1, name.into());

      let array_len = 2 * info.requests.len() as i32;
      let requests_arr = v8::Array::new(scope, array_len);
      for (i, request) in info.requests.iter().enumerate() {
        let specifier = v8::String::new_from_one_byte(
          scope,
          request.specifier.as_bytes(),
          v8::NewStringType::Normal,
        )
        .unwrap();
        requests_arr.set_index(scope, 2 * i as u32, specifier.into());
        let value = request.asserted_module_type.to_v8(scope);
        requests_arr.set_index(scope, (2 * i) as u32 + 1, value);
      }
      module_info_arr.set_index(scope, 2, requests_arr.into());

      let module_type = info.module_type.to_v8(scope);
      module_info_arr.set_index(scope, 3, module_type);

      info_arr.set_index(scope, i as u32, module_info_arr.into());
    }
    array.set_index(scope, 2, info_arr.into());

    let length = self.data.borrow().by_name.len();
    let by_name_array = v8::Array::new(scope, length.try_into().unwrap());
    self
      .data
      .borrow()
      .by_name
      .walk(|i, module_type, name, module| {
        let arr = v8::Array::new(scope, 3);

        let specifier = name.v8(scope);
        arr.set_index(scope, 0, specifier.into());
        let value = module_type.to_v8(scope);
        arr.set_index(scope, 1, value);

        let symbolic_module: v8::Local<v8::Value> = match module {
          SymbolicModule::Alias(alias) => {
            let alias = v8::String::new_from_one_byte(
              scope,
              alias.as_bytes(),
              v8::NewStringType::Normal,
            )
            .unwrap();
            alias.into()
          }
          SymbolicModule::Mod(id) => {
            let id = v8::Integer::new(scope, *id as i32);
            id.into()
          }
        };
        arr.set_index(scope, 2, symbolic_module);

        by_name_array.set_index(scope, i as u32, arr.into());
      });
    array.set_index(scope, 3, by_name_array.into());

    let array_global = v8::Global::new(scope, array);

    let handles = data.handles.clone();
    SnapshottedData {
      module_map_data: array_global,
      module_handles: handles,
    }
  }

  pub fn update_with_snapshotted_data(
    &self,
    scope: &mut v8::HandleScope,
    snapshotted_data: SnapshottedData,
  ) {
    let local_data: v8::Local<v8::Array> =
      v8::Local::new(scope, snapshotted_data.module_map_data);

    {
      let next_load_id = local_data.get_index(scope, 0).unwrap();
      assert!(next_load_id.is_int32());
      let integer = next_load_id.to_integer(scope).unwrap();
      let val = integer.int32_value(scope).unwrap();
      self.data.borrow_mut().next_load_id = val;
    }

    {
      let main_module_id = local_data.get_index(scope, 1).unwrap();
      if !main_module_id.is_undefined() {
        let integer = main_module_id
          .to_integer(scope)
          .map(|v| v.int32_value(scope).unwrap() as usize);
        self.data.borrow_mut().main_module_id = integer;
      }
    }

    {
      let info_val = local_data.get_index(scope, 2).unwrap();

      let info_arr: v8::Local<v8::Array> = info_val.try_into().unwrap();
      let len = info_arr.length() as usize;
      // Over allocate so executing a few scripts doesn't have to resize this vec.
      let mut info = Vec::with_capacity(len + 16);

      for i in 0..len {
        let module_info_arr: v8::Local<v8::Array> = info_arr
          .get_index(scope, i as u32)
          .unwrap()
          .try_into()
          .unwrap();
        let id = module_info_arr
          .get_index(scope, 0)
          .unwrap()
          .to_integer(scope)
          .unwrap()
          .value() as ModuleId;

        let name = module_info_arr
          .get_index(scope, 1)
          .unwrap()
          .to_rust_string_lossy(scope)
          .into();

        let requests_arr: v8::Local<v8::Array> = module_info_arr
          .get_index(scope, 2)
          .unwrap()
          .try_into()
          .unwrap();
        let len = (requests_arr.length() as usize) / 2;
        let mut requests = Vec::with_capacity(len);
        for i in 0..len {
          let specifier = requests_arr
            .get_index(scope, (2 * i) as u32)
            .unwrap()
            .to_rust_string_lossy(scope);
          let value =
            requests_arr.get_index(scope, (2 * i + 1) as u32).unwrap();
          let asserted_module_type =
            AssertedModuleType::try_from_v8(scope, value).unwrap();
          requests.push(ModuleRequest {
            specifier,
            asserted_module_type,
          });
        }

        let value = module_info_arr.get_index(scope, 3).unwrap();
        let module_type =
          AssertedModuleType::try_from_v8(scope, value).unwrap();

        let main = self.data.borrow().main_module_id == Some(id);
        let module_info = ModuleInfo {
          id,
          main,
          name,
          requests,
          module_type,
        };
        info.push(module_info);
      }

      self.data.borrow_mut().info = info;
    }

    self.data.borrow_mut().by_name.clear();

    {
      let by_name_arr: v8::Local<v8::Array> =
        local_data.get_index(scope, 3).unwrap().try_into().unwrap();
      let len = by_name_arr.length() as usize;

      for i in 0..len {
        let arr: v8::Local<v8::Array> = by_name_arr
          .get_index(scope, i as u32)
          .unwrap()
          .try_into()
          .unwrap();

        let specifier =
          arr.get_index(scope, 0).unwrap().to_rust_string_lossy(scope);
        let asserted_module_type = match arr
          .get_index(scope, 1)
          .unwrap()
          .to_integer(scope)
          .unwrap()
          .value()
        {
          0 => AssertedModuleType::JavaScriptOrWasm,
          1 => AssertedModuleType::Json,
          _ => unreachable!(),
        };

        let symbolic_module_val = arr.get_index(scope, 2).unwrap();
        let val = if symbolic_module_val.is_number() {
          SymbolicModule::Mod(
            symbolic_module_val
              .to_integer(scope)
              .unwrap()
              .value()
              .try_into()
              .unwrap(),
          )
        } else {
          SymbolicModule::Alias(
            symbolic_module_val.to_rust_string_lossy(scope).into(),
          )
        };

        self.data.borrow_mut().by_name.insert(
          &asserted_module_type,
          specifier.into(),
          val,
        );
      }
    }

    self.data.borrow_mut().handles = snapshotted_data.module_handles;
  }

  pub(crate) fn new(loader: Rc<dyn ModuleLoader>) -> ModuleMap {
    Self {
      loader: loader.into(),
      dyn_module_evaluate_idle_counter: Default::default(),
      dynamic_import_map: Default::default(),
      preparing_dynamic_imports: Default::default(),
      pending_dynamic_imports: Default::default(),
      pending_dyn_mod_evaluations: Default::default(),
      data: Default::default(),
    }
  }

  /// Get module id, following all aliases in case of module specifier
  /// that had been redirected.
  pub(crate) fn get_id(
    &self,
    name: impl AsRef<str>,
    asserted_module_type: impl AsRef<AssertedModuleType>,
  ) -> Option<ModuleId> {
    let map = &self.data.borrow().by_name;
    let first_symbolic_module =
      map.get(asserted_module_type.as_ref(), name.as_ref())?;
    let mut mod_name = match first_symbolic_module {
      SymbolicModule::Mod(mod_id) => return Some(*mod_id),
      SymbolicModule::Alias(target) => target,
    };
    loop {
      let symbolic_module =
        map.get(asserted_module_type.as_ref(), mod_name.as_ref())?;
      match symbolic_module {
        SymbolicModule::Alias(target) => {
          debug_assert!(mod_name != target);
          mod_name = target;
        }
        SymbolicModule::Mod(mod_id) => return Some(*mod_id),
      }
    }
  }

  pub(crate) fn new_json_module(
    &self,
    scope: &mut v8::HandleScope,
    name: ModuleName,
    source: ModuleCode,
  ) -> Result<ModuleId, ModuleError> {
    let name_str = name.v8(scope);
    let source_str = v8::String::new_from_utf8(
      scope,
      strip_bom(source.as_bytes()),
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

    let export_names = [v8::String::new(tc_scope, "default").unwrap()];
    let module = v8::Module::create_synthetic_module(
      tc_scope,
      name_str,
      &export_names,
      json_module_evaluation_steps,
    );

    let handle = v8::Global::<v8::Module>::new(tc_scope, module);
    let value_handle = v8::Global::<v8::Value>::new(tc_scope, parsed_json);
    self
      .data
      .borrow_mut()
      .json_value_store
      .insert(handle.clone(), value_handle);

    let id =
      self.create_module_info(name, ModuleType::Json, handle, false, vec![]);

    Ok(id)
  }

  /// Create and compile an ES module.
  pub(crate) fn new_es_module(
    &self,
    scope: &mut v8::HandleScope,
    main: bool,
    name: ModuleName,
    source: ModuleCode,
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

      let import_assertions = module_request.get_import_assertions();

      let assertions = parse_import_assertions(
        tc_scope,
        import_assertions,
        ImportAssertionsKind::StaticImport,
      );

      // FIXME(bartomieju): there are no stack frames if exception
      // is thrown here
      {
        let state_rc = JsRuntime::state_from(tc_scope);
        let state = state_rc.borrow();
        (state.validate_import_attributes_cb)(tc_scope, &assertions);
      }

      if tc_scope.has_caught() {
        let exception = tc_scope.exception().unwrap();
        let exception = v8::Global::new(tc_scope, exception);
        return Err(ModuleError::Exception(exception));
      }

      let module_specifier = match self.loader.borrow().resolve(
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
      let asserted_module_type =
        get_asserted_module_type_from_assertions(&assertions);
      let request = ModuleRequest {
        specifier: module_specifier.to_string(),
        asserted_module_type,
      };
      requests.push(request);
    }

    if main {
      let data = self.data.borrow();
      if let Some(main_module) = data.main_module_id {
        let main_name = self.get_name_by_id(main_module).unwrap();
        return Err(ModuleError::Other(generic_error(
          format!("Trying to create \"main\" module ({:?}), when one already exists ({:?})",
          name.as_ref(),
          main_name,
        ))));
      }
    }

    let handle = v8::Global::<v8::Module>::new(tc_scope, module);
    let id = self.create_module_info(
      name,
      ModuleType::JavaScript,
      handle,
      main,
      requests,
    );

    Ok(id)
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
    import_assertions: v8::Local<'s, v8::FixedArray>,
    referrer: v8::Local<'s, v8::Module>,
  ) -> Option<v8::Local<'s, v8::Module>> {
    // SAFETY: `CallbackScope` can be safely constructed from `Local<Context>`
    let scope = &mut unsafe { v8::CallbackScope::new(context) };

    let module_map =
      // SAFETY: We retrieve the pointer from the slot, having just set it a few stack frames up
      unsafe { scope.get_slot::<*const Self>().unwrap().as_ref().unwrap() };

    let referrer_global = v8::Global::new(scope, referrer);

    let referrer_name = module_map
      .get_name_by_module(&referrer_global)
      .expect("ModuleInfo not found");

    let specifier_str = specifier.to_rust_string_lossy(scope);

    let assertions = parse_import_assertions(
      scope,
      import_assertions,
      ImportAssertionsKind::StaticImport,
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

  /// Called by `module_resolve_callback` during module instantiation.
  fn resolve_callback<'s>(
    &self,
    scope: &mut v8::HandleScope<'s>,
    specifier: &str,
    referrer: &str,
    import_assertions: HashMap<String, String>,
  ) -> Option<v8::Local<'s, v8::Module>> {
    let resolved_specifier = self
      .loader
      .borrow()
      .resolve(specifier, referrer, ResolutionKind::Import)
      .expect("Module should have been already resolved");

    let module_type =
      get_asserted_module_type_from_assertions(&import_assertions);

    if let Some(id) = self.get_id(resolved_specifier.as_str(), module_type) {
      if let Some(handle) = self.get_handle(id) {
        return Some(v8::Local::new(scope, handle));
      }
    }

    None
  }

  pub(crate) fn get_handle_by_name(
    &self,
    name: impl AsRef<str>,
  ) -> Option<v8::Global<v8::Module>> {
    let id = self
      .get_id(name.as_ref(), AssertedModuleType::JavaScriptOrWasm)
      .or_else(|| self.get_id(name.as_ref(), AssertedModuleType::Json))?;
    self.get_handle(id)
  }

  pub(crate) fn inject_handle(
    &self,
    name: ModuleName,
    module_type: ModuleType,
    handle: v8::Global<v8::Module>,
  ) {
    self.create_module_info(name, module_type, handle, false, vec![]);
  }

  fn create_module_info(
    &self,
    name: FastString,
    module_type: ModuleType,
    handle: v8::Global<v8::Module>,
    main: bool,
    requests: Vec<ModuleRequest>,
  ) -> ModuleId {
    let mut data = self.data.borrow_mut();
    let id = data.handles.len();
    let module_type = module_type.into();
    let (name1, name2) = name.into_cheap_copy();
    data.handles_inverted.insert(handle.clone(), id);
    data.handles.push(handle);
    if main {
      data.main_module_id = Some(id);
    }
    data
      .by_name
      .insert(&module_type, name1, SymbolicModule::Mod(id));
    data.info.push(ModuleInfo {
      id,
      main,
      name: name2,
      requests,
      module_type,
    });

    id
  }

  pub(crate) fn get_requested_modules(
    &self,
    id: ModuleId,
  ) -> Option<Vec<ModuleRequest>> {
    // TODO(mmastrac): Remove cloning. We were originally cloning this at the call sites but that's no excuse.
    self.data.borrow().info.get(id).map(|i| i.requests.clone())
  }

  fn is_registered(
    &self,
    specifier: impl AsRef<str>,
    asserted_module_type: impl AsRef<AssertedModuleType>,
  ) -> bool {
    if let Some(id) =
      self.get_id(specifier.as_ref(), asserted_module_type.as_ref())
    {
      return asserted_module_type.as_ref()
        == &self.data.borrow().info.get(id).unwrap().module_type;
    }

    false
  }

  pub(crate) fn alias(
    &self,
    name: FastString,
    asserted_module_type: &AssertedModuleType,
    target: FastString,
  ) {
    debug_assert_ne!(name, target);
    self.data.borrow_mut().by_name.insert(
      asserted_module_type,
      name,
      SymbolicModule::Alias(target),
    );
  }

  #[cfg(test)]
  pub(crate) fn is_alias(
    &self,
    name: &str,
    asserted_module_type: AssertedModuleType,
  ) -> bool {
    let map = &self.data.borrow().by_name;
    let entry = map.get(&asserted_module_type, name);
    matches!(entry, Some(SymbolicModule::Alias(_)))
  }

  pub(crate) fn get_handle(
    &self,
    id: ModuleId,
  ) -> Option<v8::Global<v8::Module>> {
    self.data.borrow().handles.get(id).cloned()
  }

  pub(crate) fn get_name_by_module(
    &self,
    global: &v8::Global<v8::Module>,
  ) -> Option<String> {
    if let Some(id) = self.data.borrow().handles_inverted.get(global) {
      self.get_name_by_id(*id)
    } else {
      None
    }
  }

  pub(crate) fn is_main_module(&self, global: &v8::Global<v8::Module>) -> bool {
    let data = self.data.borrow();
    data
      .main_module_id
      .map(|id| data.handles_inverted.get(global) == Some(&id))
      .unwrap_or_default()
  }

  pub(crate) fn get_name_by_id(&self, id: ModuleId) -> Option<String> {
    // TODO(mmastrac): Don't clone
    self
      .data
      .borrow()
      .info
      .get(id)
      .map(|info| info.name.as_str().to_owned())
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
    module_map_rc: Rc<ModuleMap>,
    specifier: &str,
    referrer: &str,
    asserted_module_type: AssertedModuleType,
    resolver_handle: v8::Global<v8::PromiseResolver>,
  ) {
    let load = RecursiveModuleLoad::dynamic_import(
      specifier,
      referrer,
      asserted_module_type.clone(),
      module_map_rc.clone(),
    );

    module_map_rc
      .dynamic_import_map
      .borrow_mut()
      .insert(load.id, resolver_handle);

    let loader = module_map_rc.loader.clone();
    let resolve_result = loader.borrow().resolve(
      specifier,
      referrer,
      ResolutionKind::DynamicImport,
    );
    let fut = match resolve_result {
      Ok(module_specifier) => {
        if module_map_rc.is_registered(module_specifier, asserted_module_type) {
          async move { (load.id, Ok(load)) }.boxed_local()
        } else {
          async move { (load.id, load.prepare().await.map(|()| load)) }
            .boxed_local()
        }
      }
      Err(error) => async move { (load.id, Err(error)) }.boxed_local(),
    };
    module_map_rc
      .preparing_dynamic_imports
      .borrow_mut()
      .push(fut);
  }

  pub(crate) fn has_pending_dynamic_imports(&self) -> bool {
    !(self.preparing_dynamic_imports.borrow().is_empty()
      && self.pending_dynamic_imports.borrow().is_empty())
  }

  pub(crate) fn has_pending_dyn_module_evaluation(&self) -> bool {
    !self.pending_dyn_mod_evaluations.borrow().is_empty()
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
      assert!(
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
    } else if tc_scope.has_terminated() || tc_scope.is_execution_terminating() {
      return Err(
        generic_error("Cannot evaluate dynamically imported module, because JavaScript execution has been terminated.")
      );
    } else {
      assert!(status == v8::ModuleStatus::Errored);
    }

    Ok(())
  }

  // Returns true if some dynamic import was resolved.
  pub(crate) fn evaluate_dyn_imports(
    &self,
    scope: &mut v8::HandleScope,
  ) -> bool {
    let pending =
      std::mem::take(self.pending_dyn_mod_evaluations.borrow_mut().deref_mut());
    if pending.is_empty() {
      return false;
    }
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

  pub(crate) fn poll_prepare_dyn_imports(
    &self,
    scope: &mut v8::HandleScope,
    cx: &mut Context,
  ) -> Poll<Result<(), Error>> {
    if self.preparing_dynamic_imports.borrow().is_empty() {
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
      return Poll::Ready(Ok(()));
    }
  }

  pub(crate) fn poll_dyn_imports(
    &self,
    scope: &mut v8::HandleScope,
    cx: &mut Context,
  ) -> Poll<Result<(), Error>> {
    if self.pending_dynamic_imports.borrow().is_empty() {
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
    let module_handle =
      self.get_handle(module_id).expect("ModuleInfo not found");

    let module = module_handle.open(scope);

    if module.get_status() == v8::ModuleStatus::Errored {
      let exception = module.get_exception();
      return exception_to_err_result(scope, exception, false);
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

  // TODO(mmastrac): this is better than giving the entire crate access to the internals.
  #[cfg(test)]
  pub fn assert_module_map(&self, modules: &Vec<ModuleInfo>) {
    let data = self.data.borrow();
    assert_eq!(data.handles.len(), modules.len());
    assert_eq!(data.info.len(), modules.len());
    assert_eq!(data.next_load_id as usize, modules.len());
    assert_eq!(data.by_name.len(), modules.len());

    for info in modules {
      let data = self.data.borrow();
      assert!(data.handles.get(info.id).is_some());
      assert_eq!(data.info.get(info.id).unwrap(), info);
      assert_eq!(
        data
          .by_name
          .get(&AssertedModuleType::JavaScriptOrWasm, &info.name),
        Some(&SymbolicModule::Mod(info.id))
      );
    }
  }
}

impl Default for ModuleMap {
  fn default() -> Self {
    Self::new(Rc::new(NoopModuleLoader))
  }
}

// Clippy thinks the return value doesn't need to be an Option, it's unaware
// of the mapping that MapFnFrom<F> does for ResolveModuleCallback.
#[allow(clippy::unnecessary_wraps)]
fn json_module_evaluation_steps<'a>(
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
    .json_value_store
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
