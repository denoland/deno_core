// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
use serde::ser;
use serde::ser::Serialize;

use std::cell::RefCell;
use std::ops::DerefMut;

use crate::error::Error;
use crate::error::Result;
use crate::keys::v8_struct_key;
use crate::magic;
use crate::magic::transl8::opaque_deref_mut;
use crate::magic::transl8::opaque_recv;
use crate::magic::transl8::MagicType;
use crate::magic::transl8::ToV8;
use crate::magic::transl8::MAGIC_FIELD;
use crate::AnyValue;
use crate::BigInt;
use crate::ByteString;
use crate::DetachedBuffer;
use crate::ExternalPointer;
use crate::ToJsBuffer;
use crate::U16String;

type JsValue<'s> = v8::Local<'s, v8::Value>;
type JsResult<'s> = Result<JsValue<'s>>;

type ScopePtr<'a, 'b, 'c> = &'c RefCell<&'b mut v8::HandleScope<'a>>;

pub fn to_v8<'a, T>(scope: &mut v8::HandleScope<'a>, input: T) -> JsResult<'a>
where
  T: Serialize,
{
  let scopeptr = RefCell::new(scope);
  let serializer = Serializer::new(&scopeptr);

  input.serialize(serializer)
}

/// Wraps other serializers into an enum tagged variant form.
/// Uses {"Variant": ...payload...} for compatibility with serde-json.
pub struct VariantSerializer<'a, 'b, 'c, S> {
  inner: S,
  scope: ScopePtr<'a, 'b, 'c>,
  variant: &'static str,
}

impl<'a, 'b, 'c, S> VariantSerializer<'a, 'b, 'c, S> {
  pub fn new(
    scope: ScopePtr<'a, 'b, 'c>,
    variant: &'static str,
    inner: S,
  ) -> Self {
    Self {
      inner,
      scope,
      variant,
    }
  }

  fn end(self, inner: impl FnOnce(S) -> JsResult<'a>) -> JsResult<'a> {
    let value = inner(self.inner)?;
    let scope = &mut *self.scope.borrow_mut();
    let null = v8::null(scope).into();
    let key = v8_struct_key(scope, self.variant).into();
    let obj =
      v8::Object::with_prototype_and_properties(scope, null, &[key], &[value]);
    Ok(obj.into())
  }
}

impl<'a, 'b, 'c, S> ser::SerializeTupleVariant
  for VariantSerializer<'a, 'b, 'c, S>
where
  S: ser::SerializeTupleStruct<Ok = JsValue<'a>, Error = Error>,
{
  type Ok = JsValue<'a>;
  type Error = Error;

  fn serialize_field<T: ?Sized + Serialize>(
    &mut self,
    value: &T,
  ) -> Result<()> {
    self.inner.serialize_field(value)
  }

  fn end(self) -> JsResult<'a> {
    self.end(S::end)
  }
}

impl<'a, 'b, 'c, S> ser::SerializeStructVariant
  for VariantSerializer<'a, 'b, 'c, S>
where
  S: ser::SerializeStruct<Ok = JsValue<'a>, Error = Error>,
{
  type Ok = JsValue<'a>;
  type Error = Error;

  fn serialize_field<T: ?Sized + Serialize>(
    &mut self,
    key: &'static str,
    value: &T,
  ) -> Result<()> {
    self.inner.serialize_field(key, value)
  }

  fn end(self) -> JsResult<'a> {
    self.end(S::end)
  }
}

pub struct ArraySerializer<'a, 'b, 'c> {
  pending: Vec<JsValue<'a>>,
  scope: ScopePtr<'a, 'b, 'c>,
}

impl<'a, 'b, 'c> ArraySerializer<'a, 'b, 'c> {
  pub fn new(scope: ScopePtr<'a, 'b, 'c>, len: Option<usize>) -> Self {
    let pending = match len {
      Some(len) => Vec::with_capacity(len),
      None => vec![],
    };
    Self { pending, scope }
  }
}

impl<'a, 'b, 'c> ser::SerializeSeq for ArraySerializer<'a, 'b, 'c> {
  type Ok = JsValue<'a>;
  type Error = Error;

  fn serialize_element<T: ?Sized + Serialize>(
    &mut self,
    value: &T,
  ) -> Result<()> {
    let x = value.serialize(Serializer::new(self.scope))?;
    self.pending.push(x);
    Ok(())
  }

  fn end(self) -> JsResult<'a> {
    let elements = self.pending.iter().as_slice();
    let scope = &mut *self.scope.borrow_mut();
    let arr = v8::Array::new_with_elements(scope, elements);
    Ok(arr.into())
  }
}

impl<'a, 'b, 'c> ser::SerializeTuple for ArraySerializer<'a, 'b, 'c> {
  type Ok = JsValue<'a>;
  type Error = Error;

  fn serialize_element<T: ?Sized + Serialize>(
    &mut self,
    value: &T,
  ) -> Result<()> {
    ser::SerializeSeq::serialize_element(self, value)
  }

  fn end(self) -> JsResult<'a> {
    ser::SerializeSeq::end(self)
  }
}

impl<'a, 'b, 'c> ser::SerializeTupleStruct for ArraySerializer<'a, 'b, 'c> {
  type Ok = JsValue<'a>;
  type Error = Error;

  fn serialize_field<T: ?Sized + Serialize>(
    &mut self,
    value: &T,
  ) -> Result<()> {
    ser::SerializeTuple::serialize_element(self, value)
  }

  fn end(self) -> JsResult<'a> {
    ser::SerializeTuple::end(self)
  }
}

pub struct ObjectSerializer<'a, 'b, 'c> {
  scope: ScopePtr<'a, 'b, 'c>,
  keys: Vec<v8::Local<'a, v8::Name>>,
  values: Vec<JsValue<'a>>,
}

impl<'a, 'b, 'c> ObjectSerializer<'a, 'b, 'c> {
  pub fn new(scope: ScopePtr<'a, 'b, 'c>, len: usize) -> Self {
    let keys = Vec::with_capacity(len);
    let values = Vec::with_capacity(len);
    Self {
      scope,
      keys,
      values,
    }
  }
}

impl<'a, 'b, 'c> ser::SerializeStruct for ObjectSerializer<'a, 'b, 'c> {
  type Ok = JsValue<'a>;
  type Error = Error;

  fn serialize_field<T: ?Sized + Serialize>(
    &mut self,
    key: &'static str,
    value: &T,
  ) -> Result<()> {
    let value = value.serialize(Serializer::new(self.scope))?;
    let scope = &mut *self.scope.borrow_mut();
    let key = v8_struct_key(scope, key).into();
    self.keys.push(key);
    self.values.push(value);
    Ok(())
  }

  fn end(self) -> JsResult<'a> {
    let scope = &mut *self.scope.borrow_mut();
    let null = v8::null(scope);
    let obj = v8::Object::with_prototype_and_properties(
      scope,
      null.into(),
      &self.keys[..],
      &self.values[..],
    );
    Ok(obj.into())
  }
}

pub struct MagicalSerializer<'a, 'b, 'c, T> {
  scope: ScopePtr<'a, 'b, 'c>,
  opaque: u64,
  p1: std::marker::PhantomData<T>,
}

impl<'a, 'b, 'c, T> MagicalSerializer<'a, 'b, 'c, T> {
  pub fn new(scope: ScopePtr<'a, 'b, 'c>) -> MagicalSerializer<'a, 'b, 'c, T> {
    Self {
      scope,
      opaque: 0,
      p1: std::marker::PhantomData::<T> {},
    }
  }
}

impl<'a, 'b, 'c, T: MagicType + ToV8> ser::SerializeStruct
  for MagicalSerializer<'a, 'b, 'c, T>
{
  type Ok = JsValue<'a>;
  type Error = Error;

  fn serialize_field<U: ?Sized + Serialize>(
    &mut self,
    key: &'static str,
    value: &U,
  ) -> Result<()> {
    assert_eq!(key, MAGIC_FIELD);
    let ptr: &U = value;
    // SAFETY: MagicalSerializer only ever receives single field u64s,
    // type-safety is ensured by MAGIC_NAME checks in `serialize_struct()`
    self.opaque = unsafe { opaque_recv(ptr) };
    Ok(())
  }

  fn end(self) -> JsResult<'a> {
    // SAFETY: transerialization assumptions imply `T` is still alive.
    let x: &mut T = unsafe { opaque_deref_mut(self.opaque) };
    let scope = &mut *self.scope.borrow_mut();
    x.to_v8(scope)
  }
}

// Dispatches between magic and regular struct serializers
pub enum StructSerializers<'a, 'b, 'c> {
  ExternalPointer(MagicalSerializer<'a, 'b, 'c, magic::ExternalPointer>),
  Magic(MagicalSerializer<'a, 'b, 'c, magic::Value<'a>>),
  RustToV8Buf(MagicalSerializer<'a, 'b, 'c, ToJsBuffer>),
  MagicAnyValue(MagicalSerializer<'a, 'b, 'c, AnyValue>),
  MagicDetached(MagicalSerializer<'a, 'b, 'c, DetachedBuffer>),
  MagicByteString(MagicalSerializer<'a, 'b, 'c, ByteString>),
  MagicU16String(MagicalSerializer<'a, 'b, 'c, U16String>),
  MagicBigInt(MagicalSerializer<'a, 'b, 'c, BigInt>),
  Regular(ObjectSerializer<'a, 'b, 'c>),
}

impl<'a, 'b, 'c> ser::SerializeStruct for StructSerializers<'a, 'b, 'c> {
  type Ok = JsValue<'a>;
  type Error = Error;

  fn serialize_field<T: ?Sized + Serialize>(
    &mut self,
    key: &'static str,
    value: &T,
  ) -> Result<()> {
    match self {
      StructSerializers::ExternalPointer(s) => s.serialize_field(key, value),
      StructSerializers::Magic(s) => s.serialize_field(key, value),
      StructSerializers::RustToV8Buf(s) => s.serialize_field(key, value),
      StructSerializers::MagicAnyValue(s) => s.serialize_field(key, value),
      StructSerializers::MagicDetached(s) => s.serialize_field(key, value),
      StructSerializers::MagicByteString(s) => s.serialize_field(key, value),
      StructSerializers::MagicU16String(s) => s.serialize_field(key, value),
      StructSerializers::MagicBigInt(s) => s.serialize_field(key, value),
      StructSerializers::Regular(s) => s.serialize_field(key, value),
    }
  }

  fn end(self) -> JsResult<'a> {
    match self {
      StructSerializers::ExternalPointer(s) => s.end(),
      StructSerializers::Magic(s) => s.end(),
      StructSerializers::RustToV8Buf(s) => s.end(),
      StructSerializers::MagicAnyValue(s) => s.end(),
      StructSerializers::MagicDetached(s) => s.end(),
      StructSerializers::MagicByteString(s) => s.end(),
      StructSerializers::MagicU16String(s) => s.end(),
      StructSerializers::MagicBigInt(s) => s.end(),
      StructSerializers::Regular(s) => s.end(),
    }
  }
}

// Serializes to JS Objects, NOT JS Maps ...
pub struct MapSerializer<'a, 'b, 'c> {
  scope: ScopePtr<'a, 'b, 'c>,
  keys: Vec<v8::Local<'a, v8::Name>>,
  values: Vec<JsValue<'a>>,
}

impl<'a, 'b, 'c> MapSerializer<'a, 'b, 'c> {
  pub fn new(scope: ScopePtr<'a, 'b, 'c>, len: Option<usize>) -> Self {
    let keys = Vec::with_capacity(len.unwrap_or_default());
    let values = Vec::with_capacity(len.unwrap_or_default());
    Self {
      scope,
      keys,
      values,
    }
  }
}

impl<'a, 'b, 'c> ser::SerializeMap for MapSerializer<'a, 'b, 'c> {
  type Ok = JsValue<'a>;
  type Error = Error;

  fn serialize_key<T: ?Sized + Serialize>(&mut self, key: &T) -> Result<()> {
    let key = key.serialize(Serializer::new(self.scope))?;
    self.keys.push(key.try_into().map_err(|_| {
      Error::Message("Serialized Maps expect String keys".into())
    })?);
    Ok(())
  }

  fn serialize_value<T: ?Sized + Serialize>(
    &mut self,
    value: &T,
  ) -> Result<()> {
    let v8_value = value.serialize(Serializer::new(self.scope))?;
    self.values.push(v8_value);
    Ok(())
  }

  fn end(self) -> JsResult<'a> {
    debug_assert!(self.keys.len() == self.values.len());
    let scope = &mut *self.scope.borrow_mut();
    let null = v8::null(scope).into();
    let obj = v8::Object::with_prototype_and_properties(
      scope,
      null,
      &self.keys[..],
      &self.values[..],
    );
    Ok(obj.into())
  }
}

pub struct Serializer<'a, 'b, 'c> {
  scope: ScopePtr<'a, 'b, 'c>,
}

impl<'a, 'b, 'c> Serializer<'a, 'b, 'c> {
  pub fn new(scope: ScopePtr<'a, 'b, 'c>) -> Self {
    Serializer { scope }
  }
}

macro_rules! forward_to {
    ($($name:ident($ty:ty, $to:ident, $lt:lifetime);)*) => {
        $(fn $name(self, v: $ty) -> JsResult<$lt> {
            self.$to(v as _)
        })*
    };
}

pub(crate) const MAX_SAFE_INTEGER: i64 = (1 << 53) - 1;
pub(crate) const MIN_SAFE_INTEGER: i64 = -MAX_SAFE_INTEGER;

impl<'a, 'b, 'c> ser::Serializer for Serializer<'a, 'b, 'c> {
  type Ok = v8::Local<'a, v8::Value>;
  type Error = Error;

  type SerializeSeq = ArraySerializer<'a, 'b, 'c>;
  type SerializeTuple = ArraySerializer<'a, 'b, 'c>;
  type SerializeTupleStruct = ArraySerializer<'a, 'b, 'c>;
  type SerializeTupleVariant =
    VariantSerializer<'a, 'b, 'c, ArraySerializer<'a, 'b, 'c>>;
  type SerializeMap = MapSerializer<'a, 'b, 'c>;
  type SerializeStruct = StructSerializers<'a, 'b, 'c>;
  type SerializeStructVariant =
    VariantSerializer<'a, 'b, 'c, StructSerializers<'a, 'b, 'c>>;

  forward_to! {
      serialize_i8(i8, serialize_i32, 'a);
      serialize_i16(i16, serialize_i32, 'a);

      serialize_u8(u8, serialize_u32, 'a);
      serialize_u16(u16, serialize_u32, 'a);

      serialize_f32(f32, serialize_f64, 'a);
  }

  fn serialize_i32(self, v: i32) -> JsResult<'a> {
    Ok(v8::Integer::new(&mut self.scope.borrow_mut(), v).into())
  }

  fn serialize_u32(self, v: u32) -> JsResult<'a> {
    Ok(v8::Integer::new_from_unsigned(&mut self.scope.borrow_mut(), v).into())
  }

  fn serialize_i64(self, v: i64) -> JsResult<'a> {
    let s = &mut self.scope.borrow_mut();
    // If i64 can fit in max safe integer bounds then serialize as v8::Number
    // otherwise serialize as v8::BigInt
    if (MIN_SAFE_INTEGER..=MAX_SAFE_INTEGER).contains(&v) {
      Ok(v8::Number::new(s, v as _).into())
    } else {
      Ok(v8::BigInt::new_from_i64(s, v).into())
    }
  }

  fn serialize_u64(self, v: u64) -> JsResult<'a> {
    let s = &mut self.scope.borrow_mut();
    // If u64 can fit in max safe integer bounds then serialize as v8::Number
    // otherwise serialize as v8::BigInt
    if v <= (MAX_SAFE_INTEGER as u64) {
      Ok(v8::Number::new(s, v as _).into())
    } else {
      Ok(v8::BigInt::new_from_u64(s, v).into())
    }
  }

  fn serialize_f64(self, v: f64) -> JsResult<'a> {
    let scope = &mut self.scope.borrow_mut();
    Ok(v8::Number::new(scope.deref_mut(), v).into())
  }

  fn serialize_bool(self, v: bool) -> JsResult<'a> {
    Ok(v8::Boolean::new(&mut *self.scope.borrow_mut(), v).into())
  }

  fn serialize_char(self, v: char) -> JsResult<'a> {
    self.serialize_str(&v.to_string())
  }

  fn serialize_str(self, v: &str) -> JsResult<'a> {
    let maybe_str = v8::String::new(&mut self.scope.borrow_mut(), v);

    // v8 string can return 'None' if buffer length > kMaxLength.
    if let Some(str) = maybe_str {
      Ok(str.into())
    } else {
      Err(Error::Message(String::from(
        "Cannot allocate String: buffer exceeds maximum length.",
      )))
    }
  }

  fn serialize_bytes(self, v: &[u8]) -> JsResult<'a> {
    Ok(slice_to_uint8array(&mut self.scope.borrow_mut(), v).into())
  }

  fn serialize_none(self) -> JsResult<'a> {
    Ok(v8::null(&mut *self.scope.borrow_mut()).into())
  }

  fn serialize_some<T: ?Sized + Serialize>(self, value: &T) -> JsResult<'a> {
    value.serialize(self)
  }

  fn serialize_unit(self) -> JsResult<'a> {
    Ok(v8::null(&mut *self.scope.borrow_mut()).into())
  }

  fn serialize_unit_struct(self, _name: &'static str) -> JsResult<'a> {
    Ok(v8::null(&mut *self.scope.borrow_mut()).into())
  }

  /// For compatibility with serde-json, serialises unit variants as "Variant" strings.
  fn serialize_unit_variant(
    self,
    _name: &'static str,
    _variant_index: u32,
    variant: &'static str,
  ) -> JsResult<'a> {
    Ok(v8_struct_key(&mut self.scope.borrow_mut(), variant).into())
  }

  fn serialize_newtype_struct<T: ?Sized + Serialize>(
    self,
    _name: &'static str,
    value: &T,
  ) -> JsResult<'a> {
    value.serialize(self)
  }

  fn serialize_newtype_variant<T: ?Sized + Serialize>(
    self,
    _name: &'static str,
    _variant_index: u32,
    variant: &'static str,
    value: &T,
  ) -> JsResult<'a> {
    let scope = self.scope;
    let x = self.serialize_newtype_struct(variant, value)?;
    VariantSerializer::new(scope, variant, x).end(Ok)
  }

  /// Serialises any Rust iterable into a JS Array
  fn serialize_seq(self, len: Option<usize>) -> Result<Self::SerializeSeq> {
    Ok(ArraySerializer::new(self.scope, len))
  }

  fn serialize_tuple(self, len: usize) -> Result<Self::SerializeTuple> {
    self.serialize_seq(Some(len))
  }

  fn serialize_tuple_struct(
    self,
    _name: &'static str,
    len: usize,
  ) -> Result<Self::SerializeTupleStruct> {
    self.serialize_tuple(len)
  }

  fn serialize_tuple_variant(
    self,
    _name: &'static str,
    _variant_index: u32,
    variant: &'static str,
    len: usize,
  ) -> Result<Self::SerializeTupleVariant> {
    Ok(VariantSerializer::new(
      self.scope,
      variant,
      self.serialize_tuple_struct(variant, len)?,
    ))
  }

  fn serialize_map(self, len: Option<usize>) -> Result<Self::SerializeMap> {
    // Serializes a rust Map (e.g: BTreeMap, HashMap) to a v8 Object
    // TODO: consider allowing serializing to v8 Maps (e.g: via a magic type)
    // since they're lighter and better suited for K/V data
    // and maybe restrict keys (e.g: strings and numbers)
    Ok(MapSerializer::new(self.scope, len))
  }

  /// Serialises Rust typed structs into plain JS objects.
  fn serialize_struct(
    self,
    name: &'static str,
    len: usize,
  ) -> Result<Self::SerializeStruct> {
    match name {
      magic::ExternalPointer::MAGIC_NAME => {
        let m = MagicalSerializer::<ExternalPointer>::new(self.scope);
        Ok(StructSerializers::ExternalPointer(m))
      }
      ByteString::MAGIC_NAME => {
        let m = MagicalSerializer::<ByteString>::new(self.scope);
        Ok(StructSerializers::MagicByteString(m))
      }
      U16String::MAGIC_NAME => {
        let m = MagicalSerializer::<U16String>::new(self.scope);
        Ok(StructSerializers::MagicU16String(m))
      }
      ToJsBuffer::MAGIC_NAME => {
        let m = MagicalSerializer::<ToJsBuffer>::new(self.scope);
        Ok(StructSerializers::RustToV8Buf(m))
      }
      AnyValue::MAGIC_NAME => {
        let m = MagicalSerializer::<AnyValue>::new(self.scope);
        Ok(StructSerializers::MagicAnyValue(m))
      }
      DetachedBuffer::MAGIC_NAME => {
        let m = MagicalSerializer::<DetachedBuffer>::new(self.scope);
        Ok(StructSerializers::MagicDetached(m))
      }
      BigInt::MAGIC_NAME => {
        let m = MagicalSerializer::<BigInt>::new(self.scope);
        Ok(StructSerializers::MagicBigInt(m))
      }
      magic::Value::MAGIC_NAME => {
        let m = MagicalSerializer::<magic::Value<'a>>::new(self.scope);
        Ok(StructSerializers::Magic(m))
      }
      _ => {
        // Regular structs
        let o = ObjectSerializer::new(self.scope, len);
        Ok(StructSerializers::Regular(o))
      }
    }
  }

  fn serialize_struct_variant(
    self,
    _name: &'static str,
    _variant_index: u32,
    variant: &'static str,
    len: usize,
  ) -> Result<Self::SerializeStructVariant> {
    let scope = self.scope;
    let x = self.serialize_struct(variant, len)?;
    Ok(VariantSerializer::new(scope, variant, x))
  }
}

pub fn slice_to_uint8array<'a>(
  scope: &mut v8::HandleScope<'a>,
  buf: &[u8],
) -> v8::Local<'a, v8::Uint8Array> {
  let buffer = if buf.is_empty() {
    v8::ArrayBuffer::new(scope, 0)
  } else {
    let store: v8::UniqueRef<_> =
      v8::ArrayBuffer::new_backing_store(scope, buf.len());
    // SAFETY: raw memory copy into the v8 ArrayBuffer allocated above
    unsafe {
      std::ptr::copy_nonoverlapping(
        buf.as_ptr(),
        store.data().unwrap().as_ptr() as *mut u8,
        buf.len(),
      )
    }
    v8::ArrayBuffer::with_backing_store(scope, &store.make_shared())
  };
  v8::Uint8Array::new(scope, buffer, 0, buf.len())
    .expect("Failed to create UintArray8")
}
