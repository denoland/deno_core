// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
use crate::magic::transl8::impl_magic;
use crate::magic::transl8::FromV8;
use crate::magic::transl8::ToV8;
use std::mem::transmute;

/// serde_v8::Value is used internally to serialize/deserialize values in
/// objects and arrays. This struct was exposed to user code in the past, but
/// we don't want to do that anymore as it leads to inefficient usages - eg. wrapping
/// a V8 object in `serde_v8::Value` and then immediately unwrapping it.
//
// SAFETY: caveat emptor, the rust-compiler can no longer link lifetimes to their
// original scope, you must take special care in ensuring your handles don't
// outlive their scope.
pub struct Value<'s> {
  pub v8_value: v8::Local<'s, v8::Value>,
}
impl_magic!(Value<'_>);

impl<'s, T> From<v8::Local<'s, T>> for Value<'s>
where
  v8::Local<'s, T>: Into<v8::Local<'s, v8::Value>>,
{
  fn from(v: v8::Local<'s, T>) -> Self {
    Self { v8_value: v.into() }
  }
}

impl<'s> From<Value<'s>> for v8::Local<'s, v8::Value> {
  fn from(value: Value<'s>) -> Self {
    value.v8_value
  }
}

impl ToV8 for Value<'_> {
  fn to_v8<'a>(
    &self,
    _scope: &mut v8::HandleScope<'a>,
  ) -> Result<v8::Local<'a, v8::Value>, crate::Error> {
    // SAFETY: not fully safe, since lifetimes are detached from original scope
    Ok(unsafe {
      transmute::<v8::Local<v8::Value>, v8::Local<v8::Value>>(self.v8_value)
    })
  }
}

impl FromV8 for Value<'_> {
  fn from_v8(
    _scope: &mut v8::HandleScope,
    value: v8::Local<v8::Value>,
  ) -> Result<Self, crate::Error> {
    // SAFETY: not fully safe, since lifetimes are detached from original scope
    Ok(unsafe { transmute::<Value, Value>(value.into()) })
  }
}
