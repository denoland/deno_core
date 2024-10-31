// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

use crate::JsRuntime;
use std::any::TypeId;
pub use v8::cppgc::GarbageCollected;

const CPPGC_TAG: u16 = 1;

#[repr(C)]
struct CppGcObject<T: GarbageCollected> {
  tag: TypeId,
  member: T,
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
  let templ =
    v8::Local::new(scope, state.cppgc_template.borrow().as_ref().unwrap());
  let func = templ.get_function(scope).unwrap();
  let obj = func.new_instance(scope, &[]).unwrap();

  let heap = scope.get_cpp_heap().unwrap();

  let member = unsafe {
    v8::cppgc::make_garbage_collected(
      heap,
      CppGcObject {
        tag: TypeId::of::<T>(),
        member: t,
      },
    )
  };

  unsafe {
    v8::Object::wrap::<CPPGC_TAG, CppGcObject<T>>(scope, obj, &member);
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
pub fn try_unwrap_cppgc_object<'sc, T: GarbageCollected + 'static>(
  isolate: &mut v8::Isolate,
  val: v8::Local<'sc, v8::Value>,
) -> Option<Ptr<T>> {
  let Ok(obj): Result<v8::Local<v8::Object>, _> = val.try_into() else {
    return None;
  };

  if !obj.is_api_wrapper() {
    return None;
  }

  let obj =
    unsafe { v8::Object::unwrap::<CPPGC_TAG, CppGcObject<T>>(isolate, obj) }?;

  if obj.tag != TypeId::of::<T>() {
    return None;
  }

  Some(Ptr {
    inner: obj,
    root: None,
  })
}
