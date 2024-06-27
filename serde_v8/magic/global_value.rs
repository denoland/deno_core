// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
use crate::magic::transl8::impl_magic;
use crate::magic::transl8::FromV8;
use crate::magic::transl8::ToV8;

/// A wrapper around `v8::Global<v8::Value>` to allow for passing globals transparently through a serde boundary.
///
/// Serializing a `GlobalValue` creates a `v8::Local` from the global value, and deserializing creates a `v8::Global` from the local value.
pub struct GlobalValue {
  pub v8_value: v8::Global<v8::Value>,
}
impl_magic!(GlobalValue);

impl<T> From<v8::Global<T>> for GlobalValue
where
  v8::Global<T>: Into<v8::Global<v8::Value>>,
{
  fn from(v: v8::Global<T>) -> Self {
    Self { v8_value: v.into() }
  }
}

impl From<GlobalValue> for v8::Global<v8::Value> {
  fn from(value: GlobalValue) -> Self {
    value.v8_value
  }
}

impl ToV8 for GlobalValue {
  fn to_v8<'a>(
    &self,
    scope: &mut v8::HandleScope<'a>,
  ) -> Result<v8::Local<'a, v8::Value>, crate::Error> {
    Ok(v8::Local::new(scope, self.v8_value.clone()))
  }
}

impl FromV8 for GlobalValue {
  fn from_v8(
    scope: &mut v8::HandleScope,
    value: v8::Local<v8::Value>,
  ) -> Result<Self, crate::Error> {
    Ok(Self {
      v8_value: v8::Global::new(scope, value),
    })
  }
}
