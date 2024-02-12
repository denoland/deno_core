// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use crate::magic::transl8::impl_magic;
use crate::magic::transl8::FromV8;
use crate::magic::transl8::ToV8;
use std::mem::transmute;

/// serde_v8::Value allows passing through `v8::Value`s untouched
/// when de/serializing & allows mixing rust & v8 values in structs, tuples...
//
// SAFETY: caveat emptor, the rust-compiler can no longer link lifetimes to their
// original scope, you must take special care in ensuring your handles don't outlive their scope
pub struct Value<'s> {
  pub v8_value: v8::Local<'s, v8::Value>,
}
impl_magic!(Value<'_>);

impl<'s> Value<'s> {
  /// Converts this to a [`v8::Global`] infallibly, if this `v8` type can be converted infallibly
  /// to [`v8::Global<T>`] (eg: [`v8::Data`] or [`v8::Value`]).
  pub fn as_global<T>(&self, scope: &mut v8::HandleScope<'s>) -> v8::Global<T>
  where
    v8::Local<'s, T>: From<v8::Local<'s, v8::Value>>,
  {
    let local = v8::Local::<T>::from(self.v8_value);
    v8::Global::new(scope, local)
  }

  /// Converts this to a [`v8::Global`] fallibly.
  pub fn try_as_global<T>(
    &self,
    scope: &mut v8::HandleScope<'s>,
  ) -> Result<
    v8::Global<T>,
    <v8::Local<'s, T> as TryFrom<v8::Local<'s, v8::Value>>>::Error,
  >
  where
    v8::Local<'s, T>: TryFrom<v8::Local<'s, v8::Value>>,
  {
    let local = v8::Local::<T>::try_from(self.v8_value)?;
    Ok(v8::Global::new(scope, local))
  }
}

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
    &mut self,
    _scope: &mut v8::HandleScope<'a>,
  ) -> Result<v8::Local<'a, v8::Value>, crate::Error> {
    // SAFETY: not fully safe, since lifetimes are detached from original scope
    Ok(unsafe { transmute(self.v8_value) })
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

macro_rules! impl_try_from {
  ($($type:ident),+) => {$(
    /// Implements `TryFrom` for the given `v8` type. If the type is not compatible,
    /// returns `Err(v8::DataError)`.
    impl<'s> TryFrom<Value<'s>> for v8::Local<'s, v8::$type> {
      type Error = v8::DataError;
      fn try_from(value: Value<'s>) -> Result<Self, Self::Error> {
        value.try_into()
      }
    }
  )+};
}

impl_try_from!(
  External,
  Object,
  Array,
  ArrayBuffer,
  ArrayBufferView,
  DataView,
  TypedArray,
  BigInt64Array,
  BigUint64Array,
  Float32Array,
  Float64Array,
  Int16Array,
  Int32Array,
  Int8Array,
  Uint16Array,
  Uint32Array,
  Uint8Array,
  Uint8ClampedArray,
  BigIntObject,
  BooleanObject,
  Date,
  Function,
  Map,
  NumberObject,
  Promise,
  PromiseResolver,
  Proxy,
  RegExp,
  Set,
  SharedArrayBuffer,
  StringObject,
  SymbolObject,
  WasmMemoryObject,
  WasmModuleObject,
  Primitive,
  BigInt,
  Boolean,
  Name,
  String,
  Symbol,
  Number,
  Integer,
  Int32,
  Uint32
);

#[cfg(test)]
mod tests {
  use std::convert::Infallible;

  #[test]
  #[ignore]
  fn test_conversions_compile() {
    // SAFETY: We don't run this test -- it's just to make sure this code compiles
    #[allow(invalid_value)]
    #[allow(clippy::unnecessary_fallible_conversions)]
    unsafe {
      let value: v8::Local<v8::Function> = std::mem::zeroed();

      // TryInto compiles for non-value
      let serde_value: super::Value = value.into();
      let _: Result<v8::Local<v8::Function>, v8::DataError> =
        serde_value.try_into();

      // TryInto compiles for value
      let serde_value: super::Value = value.into();
      let _: Result<v8::Local<v8::Value>, Infallible> = serde_value.try_into();

      // Into compiles for value
      let serde_value: super::Value = value.into();
      let _: v8::Local<v8::Value> = serde_value.into();
    }
  }
}
