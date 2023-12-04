// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use super::AssertedModuleType;
use crate::fast_string::FastString;
use std::cell::RefCell;
use crate::modules::ModuleId;
use crate::modules::ModuleInfo;
use crate::modules::ModuleLoadId;
use crate::modules::ModuleName;
use crate::modules::ModuleRequest;
use crate::modules::ModuleType;
use crate::runtime::SnapshottedData;
use crate::ExtensionFileSource;
use std::collections::HashMap;
use std::rc::Rc;

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
  pub(crate) handles_inverted: HashMap<v8::Global<v8::Module>, usize>,
  /// The handles we have loaded so far, corresponding with the [`ModuleInfo`] in `info`.
  pub(crate) handles: Vec<v8::Global<v8::Module>>,
  /// The modules we have loaded so far.
  pub(crate) info: Vec<ModuleInfo>,
  /// [`ModuleName`] to [`SymbolicModule`] for modules.
  by_name: ModuleNameTypeMap<SymbolicModule>,
  /// The next ID used for a load.
  pub(crate) next_load_id: ModuleLoadId,
  /// If a main module has been loaded, points to it by index.
  pub(crate) main_module_id: Option<ModuleId>,
  /// This store is used to temporarily store data that is used
  /// to evaluate a "synthetic module".
  pub(crate) synthetic_module_value_store:
    HashMap<v8::Global<v8::Module>, v8::Global<v8::Value>>,
  pub(crate) lazy_esm_sources: Rc<RefCell<HashMap<&'static str, ExtensionFileSource>>>,
}

impl ModuleMapData {
  pub fn create_module_info(
    &mut self,
    name: FastString,
    module_type: ModuleType,
    handle: v8::Global<v8::Module>,
    main: bool,
    requests: Vec<ModuleRequest>,
  ) -> ModuleId {
    let data = self;
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

  /// Get module id, following all aliases in case of module specifier
  /// that had been redirected.
  pub fn get_id(
    &self,
    name: impl AsRef<str>,
    asserted_module_type: impl AsRef<AssertedModuleType>,
  ) -> Option<ModuleId> {
    let map = &self.by_name;
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

  pub fn is_registered(
    &self,
    specifier: impl AsRef<str>,
    asserted_module_type: impl AsRef<AssertedModuleType>,
  ) -> bool {
    self
      .get_id(specifier.as_ref(), asserted_module_type.as_ref())
      .is_some()
  }

  pub(crate) fn alias(
    &mut self,
    name: FastString,
    asserted_module_type: &AssertedModuleType,
    target: FastString,
  ) {
    debug_assert_ne!(name, target);
    self.by_name.insert(
      asserted_module_type,
      name,
      SymbolicModule::Alias(target),
    );
  }

  #[cfg(test)]
  pub(crate) fn is_alias(
    &self,
    name: &str,
    asserted_module_type: impl AsRef<AssertedModuleType>,
  ) -> bool {
    let map = &self.by_name;
    let entry = map.get(asserted_module_type.as_ref(), name);
    matches!(entry, Some(SymbolicModule::Alias(_)))
  }

  pub(crate) fn get_handle(
    &self,
    id: ModuleId,
  ) -> Option<v8::Global<v8::Module>> {
    self.handles.get(id).cloned()
  }

  pub(crate) fn get_name_by_module(
    &self,
    global: &v8::Global<v8::Module>,
  ) -> Option<String> {
    if let Some(id) = self.handles_inverted.get(global) {
      self.get_name_by_id(*id)
    } else {
      None
    }
  }

  pub(crate) fn is_main_module(&self, global: &v8::Global<v8::Module>) -> bool {
    self
      .main_module_id
      .map(|id| self.handles_inverted.get(global) == Some(&id))
      .unwrap_or_default()
  }

  pub(crate) fn get_name_by_id(&self, id: ModuleId) -> Option<String> {
    // TODO(mmastrac): Don't clone
    self.info.get(id).map(|info| info.name.as_str().to_owned())
  }

  pub fn serialize_for_snapshotting(
    &self,
    scope: &mut v8::HandleScope,
  ) -> SnapshottedData {
    let data = self;
    let array = v8::Array::new(scope, 3);

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

    let length = self.by_name.len();
    let by_name_array = v8::Array::new(scope, length.try_into().unwrap());
    self.by_name.walk(|i, module_type, name, module| {
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
    &mut self,
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
      self.next_load_id = val;
    }

    {
      let main_module_id = local_data.get_index(scope, 1).unwrap();
      if !main_module_id.is_undefined() {
        let integer = main_module_id
          .to_integer(scope)
          .map(|v| v.int32_value(scope).unwrap() as usize);
        self.main_module_id = integer;
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

        let main = self.main_module_id == Some(id);
        let module_info = ModuleInfo {
          id,
          main,
          name,
          requests,
          module_type,
        };
        info.push(module_info);
      }

      self.info = info;
    }

    self.by_name.clear();

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

        self
          .by_name
          .insert(&asserted_module_type, specifier.into(), val);
      }
    }

    self.handles = snapshotted_data.module_handles;
  }

  // TODO(mmastrac): this is better than giving the entire crate access to the internals.
  #[cfg(test)]
  pub fn assert_module_map(&self, modules: &Vec<ModuleInfo>) {
    let data = self;
    assert_eq!(data.handles.len(), modules.len());
    assert_eq!(data.info.len(), modules.len());
    assert_eq!(data.next_load_id as usize, modules.len());
    assert_eq!(data.by_name.len(), modules.len());

    for info in modules {
      assert!(data.handles.get(info.id).is_some());
      assert_eq!(data.info.get(info.id).unwrap(), info);
      assert_eq!(
        data.by_name.get(&info.module_type, &info.name),
        Some(&SymbolicModule::Mod(info.id))
      );
    }
  }
}
