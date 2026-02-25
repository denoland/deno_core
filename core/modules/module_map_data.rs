// Copyright 2018-2025 the Deno authors. MIT license.

use super::RequestedModuleType;
use crate::ModuleCodeString;
use crate::ModuleSource;
use crate::fast_string::FastString;
use crate::modules::ModuleId;
use crate::modules::ModuleInfo;
use crate::modules::ModuleLoadId;
use crate::modules::ModuleName;
use crate::modules::ModuleReference;
use crate::modules::ModuleRequest;
use crate::modules::ModuleType;
use crate::runtime::SnapshotDataId;
use crate::runtime::SnapshotLoadDataStore;
use crate::runtime::SnapshotStoreDataStore;
use serde::Deserialize;
use serde::Serialize;
use std::borrow::Borrow;
use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt::Debug;
use std::hash::Hash;
use std::rc::Rc;

/// A symbolic module entity.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub enum SymbolicModule {
  /// This module is an alias to another module.
  /// This is useful such that multiple names could point to
  /// the same underlying module (particularly due to redirects).
  Alias(ModuleName),
  /// This module associates with a V8 module by id.
  Mod(ModuleId),
}

impl Clone for SymbolicModule {
  fn clone(&self) -> Self {
    match self {
      SymbolicModule::Alias(name) => match name.try_clone() {
        Some(name) => SymbolicModule::Alias(name),
        None => SymbolicModule::Alias(ModuleName::from(name.to_string())),
      },
      SymbolicModule::Mod(module_id) => SymbolicModule::Mod(*module_id),
    }
  }
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
    FastString: Borrow<Q>,
    Q: Eq + Hash + ?Sized,
  {
    let index = self.map_index(ty)?;
    let map = self.submaps.get(index)?;
    map.get(name)
  }

  pub fn insert(
    &mut self,
    module_type: &RequestedModuleType,
    name: ModuleName,
    module: T,
  ) -> Option<T> {
    let index = match self.map_index(module_type) {
      Some(index) => index,
      None => {
        let index = self.submaps.len();
        self.map_index.insert(module_type.clone(), index);
        self.submaps.push(Default::default());
        index
      }
    };

    match self.submaps.get_mut(index).unwrap().insert(name, module) {
      None => {
        self.len += 1;
        None
      }
      Some(module) => Some(module),
    }
  }

  pub fn delete<Q>(
    &mut self,
    module_type: &RequestedModuleType,
    name: &Q,
  ) -> Option<T>
  where
    ModuleName: Borrow<Q>,
    Q: Eq + Hash + ?Sized,
  {
    let index = self.map_index(module_type)?;

    match self.submaps.get_mut(index).unwrap().remove(name) {
      Some(module) => {
        self.len -= 1;
        Some(module)
      }
      None => None,
    }
  }

  pub fn get_map(
    &self,
    module_type: &RequestedModuleType,
  ) -> Option<&HashMap<ModuleName, T>> {
    let index = match self.map_index(module_type) {
      Some(index) => index,
      None => todo!(),
    };

    self.submaps.get(index)
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

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub(crate) enum ModuleSourceKind {
  Wasm,
}

impl ModuleSourceKind {
  pub fn from_module_type(module_type: &ModuleType) -> Option<Self> {
    match module_type {
      ModuleType::Wasm => Some(Self::Wasm),
      _ => None,
    }
  }
}

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
enum ModuleSourceType {
  Loaded(ModuleType),
  Requested(RequestedModuleType),
}

#[derive(Debug, Eq, PartialEq, Hash)]
pub(crate) struct ModuleSourceKey {
  pub name: ModuleName,
  typ: ModuleSourceType,
}

impl ModuleSourceKey {
  pub fn from_reference(module_reference: &ModuleReference) -> Self {
    Self {
      name: module_reference.specifier.to_string().into(),
      typ: ModuleSourceType::Requested(
        module_reference.requested_module_type.clone(),
      ),
    }
  }

  pub fn from_loaded_source(loaded_source: &mut ModuleSource) -> Self {
    let name = if let Some(module_url_found) =
      loaded_source.cheap_copy_module_url_found()
    {
      module_url_found
    } else {
      loaded_source.cheap_copy_module_url_specified()
    };
    Self {
      name,
      typ: ModuleSourceType::Loaded(loaded_source.module_type.clone()),
    }
  }
}

#[derive(Default)]
pub(crate) struct ModuleMapData {
  /// Inverted index from module to index in `info`.
  pub(crate) handles_inverted: HashMap<v8::Global<v8::Module>, ModuleId>,
  /// The handles we have loaded so far, corresponding with the [`ModuleInfo`] in `info`.
  pub(crate) handles: Vec<v8::Global<v8::Module>>,
  pub(crate) main_module_callbacks: Vec<v8::Global<v8::Function>>,
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
    Rc<RefCell<HashMap<ModuleName, ModuleCodeString>>>,
  pub(crate) sources: HashMap<ModuleSourceKey, Rc<v8::Global<v8::Object>>>,
}

/// Snapshot-compatible representation of this data.
#[derive(Default, Serialize, Deserialize)]
pub(crate) struct ModuleMapSnapshotData {
  next_load_id: i32,
  main_module_id: Option<i32>,
  modules: Vec<ModuleInfo>,
  module_handles: Vec<SnapshotDataId>,
  main_module_callbacks: Vec<SnapshotDataId>,
  by_name: Vec<(FastString, RequestedModuleType, SymbolicModule)>,
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
  pub fn get_id<Q>(
    &self,
    name: &Q,
    requested_module_type: impl AsRef<RequestedModuleType>,
  ) -> Option<ModuleId>
  where
    ModuleName: Borrow<Q>,
    Q: Eq + Hash + ?Sized,
  {
    let map = &self.by_name;
    let first_symbolic_module =
      map.get(requested_module_type.as_ref(), name)?;
    let mut mod_name = match first_symbolic_module {
      SymbolicModule::Mod(mod_id) => return Some(*mod_id),
      SymbolicModule::Alias(target) => target,
    };
    loop {
      let symbolic_module =
        map.get(requested_module_type.as_ref(), mod_name.borrow())?;
      match symbolic_module {
        SymbolicModule::Alias(target) => {
          debug_assert!(mod_name != target);
          mod_name = target;
        }
        SymbolicModule::Mod(mod_id) => return Some(*mod_id),
      }
    }
  }

  pub fn get<Q>(
    &self,
    name: &Q,
    requested_module_type: impl AsRef<RequestedModuleType>,
  ) -> Option<&SymbolicModule>
  where
    ModuleName: Borrow<Q>,
    Q: Eq + Hash + ?Sized,
  {
    let map = &self.by_name;
    map.get(requested_module_type.as_ref(), name)
  }

  pub fn set(
    &mut self,
    name: ModuleName,
    symbolic_module: SymbolicModule,
    requested_module_type: impl AsRef<RequestedModuleType>,
  ) -> Option<SymbolicModule> {
    let map = &mut self.by_name;
    map.insert(requested_module_type.as_ref(), name, symbolic_module)
  }

  pub fn set_id(
    &mut self,
    name: ModuleName,
    id: ModuleId,
    requested_module_type: impl AsRef<RequestedModuleType>,
  ) -> Option<SymbolicModule> {
    let map = &mut self.by_name;
    map.insert(
      requested_module_type.as_ref(),
      name,
      SymbolicModule::Mod(id),
    )
  }

  pub fn delete<Q>(
    &mut self,
    name: &Q,
    requested_module_type: impl AsRef<RequestedModuleType>,
  ) -> Option<SymbolicModule>
  where
    ModuleName: Borrow<Q>,
    Q: Eq + Hash + ?Sized,
  {
    let map = &mut self.by_name;
    map.delete(requested_module_type.as_ref(), name)
  }

  pub fn alias(
    &mut self,
    name: ModuleName,
    requested_module_type: &RequestedModuleType,
    target: ModuleName,
  ) -> Option<SymbolicModule> {
    debug_assert_ne!(name, target);
    self.by_name.insert(
      requested_module_type,
      name,
      SymbolicModule::Alias(target),
    )
  }

  pub fn get_map(
    &self,
    requested_module_type: impl AsRef<RequestedModuleType>,
  ) -> Option<&HashMap<ModuleName, SymbolicModule>> {
    self.by_name.get_map(requested_module_type.as_ref())
  }

  #[cfg(test)]
  pub(crate) fn is_alias<Q>(
    &self,
    name: &Q,
    requested_module_type: impl AsRef<RequestedModuleType>,
  ) -> bool
  where
    ModuleName: Borrow<Q>,
    Q: Eq + Hash + Debug + ?Sized,
  {
    let map = &self.by_name;
    let entry = map.get(requested_module_type.as_ref(), name);
    matches!(entry, Some(SymbolicModule::Alias(_)))
  }

  pub fn get_handle(&self, id: ModuleId) -> Option<v8::Global<v8::Module>> {
    self.handles.get(id).cloned()
  }

  pub fn get_name_by_module(
    &self,
    global: &v8::Global<v8::Module>,
  ) -> Option<&ModuleName> {
    match self.handles_inverted.get(global) {
      Some(id) => self.get_name_by_id(*id),
      _ => None,
    }
  }

  pub(crate) fn get_type_by_module(
    &self,
    global: &v8::Global<v8::Module>,
  ) -> Option<ModuleType> {
    match self.handles_inverted.get(global) {
      Some(id) => {
        let info = self.info.get(*id).unwrap();
        Some(info.module_type.clone())
      }
      _ => None,
    }
  }

  pub(crate) fn get_info_by_module(
    &self,
    global: &v8::Global<v8::Module>,
  ) -> Option<&ModuleInfo> {
    match self.handles_inverted.get(global) {
      Some(id) => Some(self.info.get(*id).unwrap()),
      _ => None,
    }
  }

  pub(crate) fn is_main_module(&self, global: &v8::Global<v8::Module>) -> bool {
    self
      .main_module_id
      .map(|id| self.handles_inverted.get(global) == Some(&id))
      .unwrap_or_default()
  }

  pub fn get_name_by_id(&self, id: ModuleId) -> Option<&ModuleName> {
    self.info.get(id).map(|info| &info.name)
  }

  pub fn serialize_for_snapshotting(
    self,
    data_store: &mut SnapshotStoreDataStore,
  ) -> ModuleMapSnapshotData {
    debug_assert_eq!(self.by_name.len(), self.handles.len());
    debug_assert_eq!(self.info.len(), self.handles.len());

    let mut ser = ModuleMapSnapshotData {
      next_load_id: self.next_load_id,
      main_module_id: self.main_module_id.map(|x| x as _),
      modules: self.info,
      ..Default::default()
    };

    ser.main_module_callbacks = self
      .main_module_callbacks
      .into_iter()
      .map(|x| data_store.register(x))
      .collect();
    ser.module_handles = self
      .handles
      .into_iter()
      .map(|v| data_store.register(v))
      .collect();

    self.by_name.drain(|_, module_type, name, module| {
      ser.by_name.push((name, module_type.clone(), module));
    });

    ser
  }

  pub fn update_with_snapshotted_data(
    &mut self,
    scope: &mut v8::PinScope,
    data_store: &mut SnapshotLoadDataStore,
    data: ModuleMapSnapshotData,
  ) {
    self.next_load_id = data.next_load_id;
    self.main_module_id = data.main_module_id.map(|x| x as _);
    self.info = data.modules;
    self.handles.reserve(data.module_handles.len());
    self.handles_inverted.reserve(data.module_handles.len());
    self.main_module_callbacks = data
      .main_module_callbacks
      .into_iter()
      .map(|x| data_store.get(scope, x))
      .collect();

    for module_handle in data.module_handles {
      let id = self.handles.len();
      let module = data_store.get::<v8::Module>(scope, module_handle);
      self.handles_inverted.insert(module.clone(), id as _);
      self.handles.push(module);
    }

    for (name, module_type, module) in data.by_name {
      self.by_name.insert(&module_type, name, module);
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

#[cfg(test)]
mod tests {
  use super::*;
  use crate::ascii_str;
  use url::Url;

  #[test]
  fn module_name_map_test() {
    let mut data: ModuleNameTypeMap<usize> = ModuleNameTypeMap::default();
    data.insert(
      &RequestedModuleType::Json,
      ascii_str!("http://example.com/").into(),
      1,
    );
    assert_eq!(
      Some(&1),
      data.get(
        &RequestedModuleType::Json,
        Url::parse("http://example.com/").unwrap().as_str()
      )
    );
  }
}
