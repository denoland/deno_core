// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

use crate::JsRuntime;
use std::any::TypeId;
pub use v8::cppgc::GarbageCollected;

const CPPGC_TAG: u16 = 1;

#[repr(C)]
struct CppGcObject<T: GarbageCollected> {
  tag: TypeId,
  member: v8::cppgc::Member<T>,
}

impl<T: GarbageCollected> v8::cppgc::GarbageCollected for CppGcObject<T> {
  fn trace(&self, visitor: &v8::cppgc::Visitor) {
    visitor.trace(&self.member);
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

  wrap_object(scope, obj, t)
}

pub fn wrap_object<'a, T: GarbageCollected + 'static>(
  scope: &mut v8::HandleScope<'a>,
  obj: v8::Local<'a, v8::Object>,
  t: T
) -> v8::Local<'a, v8::Object> {
    // Print string of T
     println!("{:?}",obj.is_api_wrapper());
    let heap = scope.get_cpp_heap().unwrap();
    
    let member = unsafe {
        v8::cppgc::make_garbage_collected(
        heap,
        CppGcObject {
            tag: TypeId::of::<T>(),
            member: v8::cppgc::make_garbage_collected(heap, t),
        },
        )
    };
    
    unsafe {
        v8::Object::wrap::<CPPGC_TAG, CppGcObject<T>>(scope, obj, &member);
    }
    obj
    }

#[doc(hidden)]
#[allow(clippy::needless_lifetimes)]
pub fn try_unwrap_cppgc_object<'sc, T: GarbageCollected + 'static>(
  scope: &mut v8::HandleScope<'sc>,
  val: v8::Local<'sc, v8::Value>,
) -> v8::cppgc::Member<T> {

  println!("{:?}", val.to_rust_string_lossy(scope));
  let Ok(obj): Result<v8::Local<v8::Object>, _> = val.try_into() else {
    return v8::cppgc::Member::empty();
  };

  println!("{:?}", obj.is_api_wrapper());
  if !obj.is_api_wrapper() {
    return v8::cppgc::Member::empty();
  }

  let ptr =
    unsafe { v8::Object::unwrap::<CPPGC_TAG, CppGcObject<T>>(scope, obj) };
  let Some(obj) = ptr.borrow() else {
    return v8::cppgc::Member::empty();
  };

  if obj.tag != TypeId::of::<T>() {
    return v8::cppgc::Member::empty();
  }

  let mut h = v8::cppgc::Member::empty();
  h.set(&obj.member);
  h
}
