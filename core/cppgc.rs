// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

use crate::Resource;
use std::any::TypeId;

struct CppGcObject<T: Resource> {
  tag: TypeId,
  member: T,
}

impl<T: Resource> v8::cppgc::GarbageCollected for CppGcObject<T> {
  fn trace(&self, _: &v8::cppgc::Visitor) {
    println!("trace");
  }
}

pub(crate) const DEFAULT_CPP_GC_EMBEDDER_ID: u16 = 0xde90;

const EMBEDDER_ID_OFFSET: i32 = 0;
const SLOT_OFFSET: i32 = 1;
const FIELD_COUNT: usize = 2;

pub fn make_cppgc_object<'a, T: Resource>(
  scope: &'a mut v8::HandleScope,
  t: T,
) -> v8::Local<'a, v8::Object> {
  let templ = v8::ObjectTemplate::new(scope);
  templ.set_internal_field_count(FIELD_COUNT);

  let obj = templ.new_instance(scope).unwrap();
  let member = v8::cppgc::make_garbage_collected(
    scope.get_cpp_heap(),
    Box::new(CppGcObject {
      tag: TypeId::of::<T>(),
      member: t,
    }),
  );

  obj.set_aligned_pointer_in_internal_field(
    EMBEDDER_ID_OFFSET,
    &DEFAULT_CPP_GC_EMBEDDER_ID as *const u16 as _,
  );
  obj.set_aligned_pointer_in_internal_field(SLOT_OFFSET, member.handle as _);
  obj
}

// TODO: Inner struct in rusty_v8. Need an API to get the member ptr.
struct Obj<T> {
  _padding: [usize; 2],
  ptr: *const T,
}

pub fn unwrap_cppgc_object<'sc, T: Resource>(
  obj: v8::Local<v8::Object>,
) -> Option<&'sc T> {
  if obj.internal_field_count() != FIELD_COUNT {
    return None;
  }

  // SAFETY: This code works under the assumption that
  // there is no other way to obtain an object with
  // internal field count of 2.
  //
  // The object can only be created by `make_cppgc_object`.
  let member = unsafe {
    let ptr = obj.get_aligned_pointer_from_internal_field(SLOT_OFFSET as i32);
    let obj = &*(ptr as *const Obj<CppGcObject<T>>);
    &*obj.ptr
  };

  if member.tag != TypeId::of::<T>() {
    return None;
  }

  Some(&member.member)
}
