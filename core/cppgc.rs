// Copyright 2018-2025 the Deno authors. MIT license.

use crate::runtime::SnapshotLoadDataStore;
use crate::runtime::SnapshotStoreDataStore;
use crate::JsRuntime;
use serde::Deserialize;
use serde::Serialize;
use std::any::type_name;
use std::any::TypeId;
use std::collections::BTreeMap;
pub use v8::cppgc::GarbageCollected;

const CPPGC_TAG: u16 = 1;

#[repr(C)]
struct CppGcObject<T: GarbageCollected> {
  tag: TypeId,
  member: T,
}

/// PrototypeChain defines the position of the object in the prototype chain.
///
///   A    (index 0)
///  / \
/// B   C  (index 1)
/// |
/// D      (index 2)
///
/// where each B, C and D hold their own prototype chain.
pub trait PrototypeChain {
  fn prototype_index() -> Option<usize> {
    None
  }
}

/// A good enough number for the maximum prototype chain depth based on the
/// usage.
const MAX_PROTO_CHAIN: usize = 3;

struct DummyT;
impl GarbageCollected for DummyT {}

/// ErasedPtr is a wrapper around a `v8::cppgc::Ptr` that allows downcasting
/// to a specific type. Safety is guaranteed by the `tag` field in the
/// `CppGcObject` struct.
struct ErasedPtr {
  ptr: v8::cppgc::Ptr<CppGcObject<DummyT>>,
}

impl ErasedPtr {
  fn downcast<T: GarbageCollected + 'static>(
    &self,
  ) -> Option<v8::cppgc::Ptr<CppGcObject<T>>> {
    if self.ptr.tag == TypeId::of::<T>() {
      // Safety: The tag is always guaranteed by `wrap_object` to be the
      // correct type.
      Some(unsafe { std::mem::transmute_copy(&self.ptr) })
    } else {
      None
    }
  }
}

impl<T: GarbageCollected> From<v8::cppgc::Ptr<CppGcObject<T>>> for ErasedPtr {
  fn from(ptr: v8::cppgc::Ptr<CppGcObject<T>>) -> Self {
    debug_assert!(
      std::mem::size_of::<v8::cppgc::Ptr<CppGcObject<T>>>()
        == std::mem::size_of::<ErasedPtr>()
    );

    Self {
      // Safety: Both have the same size, representation and a tag guarantees safe
      // downcasting.
      ptr: unsafe {
        std::mem::transmute::<
          v8::cppgc::Ptr<CppGcObject<T>>,
          v8::cppgc::Ptr<CppGcObject<DummyT>>,
        >(ptr)
      },
    }
  }
}

struct PrototypeChainStore([Option<ErasedPtr>; MAX_PROTO_CHAIN]);

impl v8::cppgc::GarbageCollected for PrototypeChainStore {
  fn trace(&self, visitor: &v8::cppgc::Visitor) {
    // Trace all the objects top-down the prototype chain.
    for ptr in self.0.iter().flatten() {
      ptr.ptr.trace(visitor);
    }
  }
}

impl<T: GarbageCollected> v8::cppgc::GarbageCollected for CppGcObject<T> {
  fn trace(&self, visitor: &v8::cppgc::Visitor) {
    self.member.trace(visitor);
  }
}

pub(crate) fn cppgc_template_constructor(
  _scope: &mut v8::HandleScope,
  _args: v8::FunctionCallbackArguments,
  _rv: v8::ReturnValue,
) {
}

pub(crate) fn make_cppgc_template<'s>(
  scope: &mut v8::HandleScope<'s, ()>,
) -> v8::Local<'s, v8::FunctionTemplate> {
  v8::FunctionTemplate::new(scope, cppgc_template_constructor)
}

pub fn make_cppgc_object<'a, T: GarbageCollected + 'static>(
  scope: &mut v8::HandleScope<'a>,
  t: T,
) -> v8::Local<'a, v8::Object> {
  let state = JsRuntime::state_from(scope);
  let templates = state.function_templates.borrow();

  let obj = if let Some(templ) = templates.get::<T>() {
    let templ = v8::Local::new(scope, templ);
    let inst = templ.instance_template(scope);
    inst.new_instance(scope).unwrap()
  } else {
    let templ =
      v8::Local::new(scope, state.cppgc_template.borrow().as_ref().unwrap());
    let func = templ.get_function(scope).unwrap();
    func.new_instance(scope, &[]).unwrap()
  };

  wrap_object(scope, obj, t)
}

pub fn make_cppgc_object2<
  'a,
  T: GarbageCollected + 'static,
  S: GarbageCollected + 'static,
>(
  scope: &mut v8::HandleScope<'a>,
  t: T,
  t2: S,
) -> v8::Local<'a, v8::Object> {
  let state = JsRuntime::state_from(scope);
  let templates = state.function_templates.borrow();

  let obj = if let Some(templ) = templates.get::<T>() {
    let templ = v8::Local::new(scope, templ);
    let inst = templ.instance_template(scope);
    inst.new_instance(scope).unwrap()
  } else {
    let templ =
      v8::Local::new(scope, state.cppgc_template.borrow().as_ref().unwrap());
    let func = templ.get_function(scope).unwrap();
    func.new_instance(scope, &[]).unwrap()
  };

  wrap_object2(scope, obj, (t, t2))
}

pub fn wrap_object2<
  'a,
  T: GarbageCollected + 'static,
  S: GarbageCollected + 'static,
>(
  isolate: &mut v8::Isolate,
  obj: v8::Local<'a, v8::Object>,
  t: (T, S),
) -> v8::Local<'a, v8::Object> {
  let heap = isolate.get_cpp_heap().unwrap();

  let member = unsafe {
    v8::cppgc::make_garbage_collected(
      heap,
      PrototypeChainStore([
        Some(
          v8::cppgc::make_garbage_collected(
            heap,
            CppGcObject {
              tag: TypeId::of::<T>(),
              member: t.0,
            },
          )
          .into(),
        ),
        Some(
          v8::cppgc::make_garbage_collected(
            heap,
            CppGcObject {
              tag: TypeId::of::<S>(),
              member: t.1,
            },
          )
          .into(),
        ),
        None,
      ]),
    )
  };

  unsafe {
    v8::Object::wrap::<CPPGC_TAG, PrototypeChainStore>(isolate, obj, &member);
  }
  obj
}

// Wrap an API object (eg: `args.This()`)
pub fn wrap_object<'a, T: GarbageCollected + 'static>(
  isolate: &mut v8::Isolate,
  obj: v8::Local<'a, v8::Object>,
  t: T,
) -> v8::Local<'a, v8::Object> {
  let heap = isolate.get_cpp_heap().unwrap();

  let member = unsafe {
    v8::cppgc::make_garbage_collected(
      heap,
      PrototypeChainStore([
        Some(
          v8::cppgc::make_garbage_collected(
            heap,
            CppGcObject {
              tag: TypeId::of::<T>(),
              member: t,
            },
          )
          .into(),
        ),
        None,
        None,
      ]),
    )
  };

  unsafe {
    v8::Object::wrap::<CPPGC_TAG, PrototypeChainStore>(isolate, obj, &member);
  }
  obj
}

#[doc(hidden)]
pub struct Ptr<T: GarbageCollected> {
  inner: v8::cppgc::Ptr<CppGcObject<T>>,
  root: Option<v8::cppgc::Persistent<CppGcObject<T>>>,
}

#[doc(hidden)]
impl<T: GarbageCollected> Ptr<T> {
  /// If this pointer is used in an async function, it could leave the stack,
  /// so this method can be called to root it in the GC and keep the reference
  /// valid.
  pub fn root(&mut self) {
    if self.root.is_none() {
      self.root = Some(v8::cppgc::Persistent::new(&self.inner));
    }
  }
}

impl<T: GarbageCollected> std::ops::Deref for Ptr<T> {
  type Target = T;
  fn deref(&self) -> &T {
    &self.inner.member
  }
}

#[doc(hidden)]
#[allow(clippy::needless_lifetimes)]
pub fn try_unwrap_cppgc_object<
  'sc,
  T: GarbageCollected + PrototypeChain + 'static,
>(
  isolate: &mut v8::Isolate,
  val: v8::Local<'sc, v8::Value>,
) -> Option<Ptr<T>> {
  let Ok(obj): Result<v8::Local<v8::Object>, _> = val.try_into() else {
    return None;
  };
  if !obj.is_api_wrapper() {
    return None;
  }

  let proto_chain = unsafe {
    v8::Object::unwrap::<CPPGC_TAG, PrototypeChainStore>(isolate, obj)
  }?;

  let proto_index = T::prototype_index().unwrap_or_default();
  if proto_index >= MAX_PROTO_CHAIN {
    return None;
  }

  let obj = proto_chain.0[proto_index].as_ref()?;

  let obj = obj.downcast::<T>()?;

  Some(Ptr {
    inner: obj,
    root: None,
  })
}

#[derive(Default)]
pub struct FunctionTemplateData {
  store: BTreeMap<String, v8::Global<v8::FunctionTemplate>>,
}

#[derive(Default, Serialize, Deserialize)]
pub struct FunctionTemplateSnapshotData {
  store_handles: Vec<(String, u32)>,
}

impl FunctionTemplateData {
  pub fn insert(
    &mut self,
    key: String,
    value: v8::Global<v8::FunctionTemplate>,
  ) {
    self.store.insert(key, value);
  }

  fn get<T>(&self) -> Option<&v8::Global<v8::FunctionTemplate>> {
    self.store.get(type_name::<T>())
  }

  pub fn get_raw(
    &self,
    key: &str,
  ) -> Option<&v8::Global<v8::FunctionTemplate>> {
    self.store.get(key)
  }

  pub fn serialize_for_snapshotting(
    self,
    data_store: &mut SnapshotStoreDataStore,
  ) -> FunctionTemplateSnapshotData {
    FunctionTemplateSnapshotData {
      store_handles: self
        .store
        .into_iter()
        .map(|(k, v)| (k, data_store.register(v)))
        .collect(),
    }
  }

  pub fn update_with_snapshotted_data(
    &mut self,
    scope: &mut v8::HandleScope,
    data_store: &mut SnapshotLoadDataStore,
    data: FunctionTemplateSnapshotData,
  ) {
    self.store = data
      .store_handles
      .into_iter()
      .map(|(k, v)| (k, data_store.get::<v8::FunctionTemplate>(scope, v)))
      .collect();
  }
}

pub struct SameObject<T: GarbageCollected + 'static> {
  cell: std::cell::OnceCell<v8::Global<v8::Object>>,
  _phantom_data: std::marker::PhantomData<T>,
}
impl<T: GarbageCollected + 'static> SameObject<T> {
  #[allow(clippy::new_without_default)]
  pub fn new() -> Self {
    Self {
      cell: Default::default(),
      _phantom_data: Default::default(),
    }
  }

  pub fn get<F>(
    &self,
    scope: &mut v8::HandleScope,
    f: F,
  ) -> v8::Global<v8::Object>
  where
    F: FnOnce() -> T,
  {
    self
      .cell
      .get_or_init(|| {
        let obj = make_cppgc_object(scope, f());
        v8::Global::new(scope, obj)
      })
      .clone()
  }
}
