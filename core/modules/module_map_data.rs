// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use super::RequestedModuleType;
use crate::fast_string::FastString;
use crate::modules::ModuleId;
use crate::modules::ModuleInfo;
use crate::modules::ModuleLoadId;
use crate::modules::ModuleName;
use crate::modules::ModuleRequest;
use crate::modules::ModuleType;
use crate::runtime::SnapshotDataId;
use crate::runtime::SnapshotLoadDataStore;
use crate::runtime::SnapshotStoreDataStore;
use crate::ExtensionFileSource;
use serde::Deserialize;
use serde::Serialize;
use std::cell::RefCell;
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

/// Map of [`ModuleName`] and [`RequestedModuleType`] to a data field.
struct ModuleNameTypeMap<T> {
  submaps: Vec<HashMap<ModuleName, T>>,
  map_index: HashMap<RequestedModuleType, usize>,
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

  fn map_index(&self, ty: &RequestedModuleType) -> Option<usize> {
    self.map_index.get(ty).copied()
  }

  pub fn get<Q>(&self, ty: &RequestedModuleType, name: &Q) -> Option<&T>
  where
    ModuleName: std::borrow::Borrow<Q>,
    Q: std::cmp::Eq + std::hash::Hash + ?Sized,
  {
    let index = self.map_index(ty)?;
    let map = self.submaps.get(index)?;
    map.get(name)
  }

  pub fn clear(&mut self) {
    self.len = 0;
    self.map_index.clear();
    self.submaps.clear();
  }

  pub fn insert(
    &mut self,
    module_type: &RequestedModuleType,
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

  /// Rather than providing an iterator, we provide a drain method. This is mainly because Rust
  /// doesn't have generators.
  pub fn drain(
    mut self,
    mut f: impl FnMut(usize, &RequestedModuleType, ModuleName, T),
  ) {
    let mut i = 0;
    for (ty, value) in self.map_index.into_iter() {
      for (key, value) in self.submaps.get_mut(value).unwrap().drain() {
        f(i, &ty, key, value);
        i += 1;
      }
    }
  }
}

/// An array of tuples that provide module exports.
///
/// "default" name will make the export "default" - ie. one that can be imported
/// with `import foo from "./virtual.js"`;.
/// All other name provide "named exports" - ie. ones that can be imported like
/// so: `import { name1, name2 } from "./virtual.js`.
pub(crate) type SyntheticModuleExports =
  Vec<(v8::Global<v8::String>, v8::Global<v8::Value>)>;

// TODO(bartlomieju): add an assertion that checks for that assumption?
// If it's true we can simplify the type to be an `Option` instead of a `HashMap`.
/// This hash map is not expected to hold more than one element at a time.
/// It is a temporary store, so we can forward data to
/// `synthetic_module_evaluation_steps` callback.
pub(crate) type SyntheticModuleExportsStore =
  HashMap<v8::Global<v8::Module>, SyntheticModuleExports>;

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
  pub(crate) synthetic_module_exports_store: SyntheticModuleExportsStore,
  pub(crate) lazy_esm_sources:
    Rc<RefCell<HashMap<&'static str, ExtensionFileSource>>>,
}

/// Snapshot-compatible representation of this data.
#[derive(Default, Serialize, Deserialize)]
pub(crate) struct ModuleMapSnapshotData {
  next_load_id: i32,
  main_module_id: Option<i32>,
  modules: Vec<(i32, String, Vec<ModuleRequest>, ModuleType, SnapshotDataId)>,
  by_name: Vec<(String, RequestedModuleType, Option<String>, Option<i32>)>,
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
    let (name1, name2) = name.into_cheap_copy();
    data.handles_inverted.insert(handle.clone(), id);
    data.handles.push(handle);
    if main {
      data.main_module_id = Some(id);
    }
    // TODO(bartlomieju): verify if we can store `ModuleType` here instead
    let requested_module_type = RequestedModuleType::from(module_type.clone());
    data
      .by_name
      .insert(&requested_module_type, name1, SymbolicModule::Mod(id));
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
    requested_module_type: impl AsRef<RequestedModuleType>,
  ) -> Option<ModuleId> {
    let map = &self.by_name;
    let first_symbolic_module =
      map.get(requested_module_type.as_ref(), name.as_ref())?;
    let mut mod_name = match first_symbolic_module {
      SymbolicModule::Mod(mod_id) => return Some(*mod_id),
      SymbolicModule::Alias(target) => target,
    };
    loop {
      let symbolic_module =
        map.get(requested_module_type.as_ref(), mod_name.as_ref())?;
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
    requested_module_type: impl AsRef<RequestedModuleType>,
  ) -> bool {
    self
      .get_id(specifier.as_ref(), requested_module_type.as_ref())
      .is_some()
  }

  pub(crate) fn alias(
    &mut self,
    name: FastString,
    requested_module_type: &RequestedModuleType,
    target: FastString,
  ) {
    debug_assert_ne!(name, target);
    self.by_name.insert(
      requested_module_type,
      name,
      SymbolicModule::Alias(target),
    );
  }

  #[cfg(test)]
  pub(crate) fn is_alias(
    &self,
    name: &str,
    requested_module_type: impl AsRef<RequestedModuleType>,
  ) -> bool {
    let map = &self.by_name;
    let entry = map.get(requested_module_type.as_ref(), name);
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

  pub(crate) fn get_type_by_module(
    &self,
    global: &v8::Global<v8::Module>,
  ) -> Option<ModuleType> {
    if let Some(id) = self.handles_inverted.get(global) {
      let info = self.info.get(*id).unwrap();
      Some(info.module_type.clone())
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
    mut self,
    scope: &mut v8::HandleScope,
    data_store: &mut SnapshotStoreDataStore,
  ) -> ModuleMapSnapshotData {
    let mut ser = ModuleMapSnapshotData::default();

    ser.next_load_id = self.next_load_id;
    ser.main_module_id = self.main_module_id.map(|x| x as _);

    debug_assert_eq!(self.info.len(), self.handles.len());

    for (info, module) in self.info.into_iter().zip(self.handles) {
      let module_handle = data_store.register(module);
      ser.modules.push((
        info.id as _,
        info.name.as_str().to_owned(),
        info.requests,
        info.module_type.clone(),
        module_handle,
      ));
    }

    self.by_name.drain(|_, module_type, name, module| {
      let (alias, id) = match module {
        SymbolicModule::Alias(alias) => (Some(alias.as_str().to_owned()), None),
        SymbolicModule::Mod(id) => (None, Some(id as i32)),
      };
      ser.by_name.push((
        name.as_str().to_owned(),
        module_type.clone(),
        alias,
        id,
      ));
    });

    ser
  }

  pub fn update_with_snapshotted_data(
    &mut self,
    scope: &mut v8::HandleScope,
    data_store: &mut SnapshotLoadDataStore,
    data: ModuleMapSnapshotData,
  ) {
    self.next_load_id = data.next_load_id;
    self.main_module_id = data.main_module_id.map(|x| x as _);

    for (id, b, requests, module_type, module_handle) in data.modules {
      let module = data_store.get::<v8::Module>(scope, module_handle);
      self.handles_inverted.insert(module.clone(), id as _);
      self.handles.push(module);
      self.info.push(ModuleInfo {
        id: id as _,
        main: Some(id as usize) == self.main_module_id,
        module_type,
        name: FastString::from(b),
        requests,
      });
    }

    for (name, module_type, c, d) in data.by_name {
      match (c, d) {
        (Some(id), None) => self.by_name.insert(
          &module_type,
          FastString::from(name),
          SymbolicModule::Alias(id.into()),
        ),
        (None, Some(id)) => self.by_name.insert(
          &module_type,
          FastString::from(name),
          SymbolicModule::Mod(id as _),
        ),
        _ => unreachable!(),
      };
    }
  }

  // TODO(mmastrac): this is better than giving the entire crate access to the internals.
  #[cfg(test)]
  pub fn assert_module_map(&self, modules: &Vec<ModuleInfo>) {
    use crate::runtime::NO_OF_BUILTIN_MODULES;
    let data = self;
    assert_eq!(data.handles.len(), modules.len() + NO_OF_BUILTIN_MODULES);
    assert_eq!(data.info.len(), modules.len() + NO_OF_BUILTIN_MODULES);
    assert_eq!(data.next_load_id as usize, modules.len());
    assert_eq!(data.by_name.len(), modules.len() + NO_OF_BUILTIN_MODULES);

    for info in modules {
      assert!(data.handles.get(info.id).is_some());
      assert_eq!(data.info.get(info.id).unwrap(), info);
      let requested_module_type =
        RequestedModuleType::from(info.module_type.clone());
      assert_eq!(
        data.by_name.get(&requested_module_type, &info.name),
        Some(&SymbolicModule::Mod(info.id))
      );
    }
  }
}
