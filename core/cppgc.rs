// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

use std::any::TypeId;

struct CppGcObject<T> {
  tag: TypeId,
  member: T,
}

impl<T> v8::cppgc::GarbageCollected for CppGcObject<T> {
  fn trace(&self, _: &v8::cppgc::Visitor) {}
}

pub(crate) const DEFAULT_CPP_GC_EMBEDDER_ID: u16 = 0xde90;

const EMBEDDER_ID_OFFSET: i32 = 0;
const SLOT_OFFSET: i32 = 1;
const FIELD_COUNT: usize = 2;

pub fn make_cppgc_object<'a, T: 'static>(
  scope: &mut v8::HandleScope<'a>,
  t: T,
) -> v8::Local<'a, v8::Object> {
  let templ = v8::ObjectTemplate::new(scope);
  templ.set_internal_field_count(FIELD_COUNT);

  let obj = templ.new_instance(scope).unwrap();
  let member = v8::cppgc::make_garbage_collected(
    scope.get_cpp_heap().unwrap(),
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

pub fn try_unwrap_cppgc_object<'sc, T: 'static>(
  val: v8::Local<'sc, v8::Value>,
) -> Option<&'sc T> {
  let Ok(obj): Result<v8::Local<v8::Object>, _> = val.try_into() else {
    return None;
  };

  if obj.internal_field_count() != FIELD_COUNT {
    return None;
  }

  // SAFETY: This code works under the assumption that
  // there is no other way to obtain an object with
  // internal field count of 2.
  //
  // The object can only be created by `make_cppgc_object`.
  let member = unsafe {
    let ptr = obj.get_aligned_pointer_from_internal_field(SLOT_OFFSET);
    let obj = &*(ptr as *const v8::cppgc::InnerMember);
    obj.get::<CppGcObject<T>>()
  };

  if member.tag != TypeId::of::<T>() {
    return None;
  }

  Some(&member.member)
}
