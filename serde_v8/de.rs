// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
use serde::de::SeqAccess as _;
use serde::de::Visitor;
use serde::de::{self};
use serde::Deserialize;

use crate::error::Error;
use crate::error::Result;
use crate::keys::v8_struct_key;
use crate::keys::KeyCache;
use crate::magic;
use crate::magic::transl8::visit_magic;
use crate::magic::transl8::FromV8;
use crate::magic::transl8::MagicType;
use crate::payload::ValueType;
use crate::AnyValue;
use crate::BigInt;
use crate::ByteString;
use crate::DetachedBuffer;
use crate::JsBuffer;
use crate::StringOrBuffer;
use crate::U16String;

pub struct Deserializer<'a, 'b, 's> {
  input: v8::Local<'a, v8::Value>,
  scope: &'b mut v8::HandleScope<'s>,
  _key_cache: Option<&'b mut KeyCache>,
}

impl<'a, 'b, 's> Deserializer<'a, 'b, 's> {
  pub fn new(
    scope: &'b mut v8::HandleScope<'s>,
    input: v8::Local<'a, v8::Value>,
    key_cache: Option<&'b mut KeyCache>,
  ) -> Self {
    Deserializer {
      input,
      scope,
      _key_cache: key_cache,
    }
  }
}

// from_v8 deserializes a v8::Value into a Deserializable / rust struct
pub fn from_v8<'de, 'a, 'b, 's, T>(
  scope: &'b mut v8::HandleScope<'s>,
  input: v8::Local<'a, v8::Value>,
) -> Result<T>
where
  T: Deserialize<'de>,
{
  let mut deserializer = Deserializer::new(scope, input, None);
  let t = T::deserialize(&mut deserializer)?;
  Ok(t)
}

// like from_v8 except accepts a KeyCache to optimize struct key decoding
pub fn from_v8_cached<'de, 'a, 'b, 's, T>(
  scope: &'b mut v8::HandleScope<'s>,
  input: v8::Local<'a, v8::Value>,
  key_cache: &mut KeyCache,
) -> Result<T>
where
  T: Deserialize<'de>,
{
  let mut deserializer = Deserializer::new(scope, input, Some(key_cache));
  let t = T::deserialize(&mut deserializer)?;
  Ok(t)
}

macro_rules! deserialize_signed {
  ($dmethod:ident, $vmethod:ident, $t:tt) => {
    fn $dmethod<V>(self, visitor: V) -> Result<V::Value>
    where
      V: Visitor<'de>,
    {
      visitor.$vmethod(
        if let Ok(x) = v8::Local::<v8::Number>::try_from(self.input) {
          x.value() as $t
        } else if let Ok(x) = v8::Local::<v8::BigInt>::try_from(self.input) {
          x.i64_value().0 as $t
        } else {
          return Err(Error::ExpectedInteger(self.input.type_repr()));
        },
      )
    }
  };
}

macro_rules! deserialize_unsigned {
  ($dmethod:ident, $vmethod:ident, $t:tt) => {
    fn $dmethod<V>(self, visitor: V) -> Result<V::Value>
    where
      V: Visitor<'de>,
    {
      visitor.$vmethod(
        if let Ok(x) = v8::Local::<v8::Number>::try_from(self.input) {
          x.value() as $t
        } else if let Ok(x) = v8::Local::<v8::BigInt>::try_from(self.input) {
          x.u64_value().0 as $t
        } else {
          return Err(Error::ExpectedInteger(self.input.type_repr()));
        },
      )
    }
  };
}

impl<'de, 'a, 'b, 's, 'x> de::Deserializer<'de>
  for &'x mut Deserializer<'a, 'b, 's>
{
  type Error = Error;

  fn deserialize_any<V>(self, visitor: V) -> Result<V::Value>
  where
    V: Visitor<'de>,
  {
    match ValueType::from_v8(self.input) {
      ValueType::Null => self.deserialize_unit(visitor),
      ValueType::Bool => self.deserialize_bool(visitor),
      // Handle floats & ints separately to work with loosely-typed serde_json
      ValueType::Number => {
        if self.input.is_uint32() {
          self.deserialize_u32(visitor)
        } else if self.input.is_int32() {
          self.deserialize_i32(visitor)
        } else {
          self.deserialize_f64(visitor)
        }
      }
      ValueType::BigInt => Err(Error::UnsupportedType),
      ValueType::String => self.deserialize_string(visitor),
      ValueType::Array => self.deserialize_seq(visitor),
      ValueType::Object => self.deserialize_map(visitor),
      // Map to Vec<u8> when deserialized via deserialize_any
      // e.g: for untagged enums or StringOrBuffer
      ValueType::ArrayBufferView | ValueType::ArrayBuffer => {
        magic::v8slice::V8Slice::from_v8(&mut *self.scope, self.input)
          .and_then(|zb| visitor.visit_byte_buf(Vec::from(&*zb)))
      }
    }
  }

  fn deserialize_bool<V>(self, visitor: V) -> Result<V::Value>
  where
    V: Visitor<'de>,
  {
    // Relaxed typechecking, will map all non-true vals to false
    visitor.visit_bool(self.input.is_true())
  }

  // signed
  deserialize_signed!(deserialize_i8, visit_i8, i8);
  deserialize_signed!(deserialize_i16, visit_i16, i16);
  deserialize_signed!(deserialize_i32, visit_i32, i32);
  deserialize_signed!(deserialize_i64, visit_i64, i64);
  // unsigned
  deserialize_unsigned!(deserialize_u8, visit_u8, u8);
  deserialize_unsigned!(deserialize_u16, visit_u16, u16);
  deserialize_unsigned!(deserialize_u32, visit_u32, u32);
  deserialize_unsigned!(deserialize_u64, visit_u64, u64);

  fn deserialize_f32<V>(self, visitor: V) -> Result<V::Value>
  where
    V: Visitor<'de>,
  {
    self.deserialize_f64(visitor)
  }
  fn deserialize_f64<V>(self, visitor: V) -> Result<V::Value>
  where
    V: Visitor<'de>,
  {
    visitor.visit_f64(
      if let Ok(x) = v8::Local::<v8::Number>::try_from(self.input) {
        x.value()
      } else if let Ok(x) = v8::Local::<v8::BigInt>::try_from(self.input) {
        bigint_to_f64(x)
      } else {
        return Err(Error::ExpectedNumber(self.input.type_repr()));
      },
    )
  }

  fn deserialize_char<V>(self, visitor: V) -> Result<V::Value>
  where
    V: Visitor<'de>,
  {
    self.deserialize_str(visitor)
  }

  fn deserialize_str<V>(self, visitor: V) -> Result<V::Value>
  where
    V: Visitor<'de>,
  {
    self.deserialize_string(visitor)
  }

  fn deserialize_string<V>(self, visitor: V) -> Result<V::Value>
  where
    V: Visitor<'de>,
  {
    if self.input.is_string() || self.input.is_string_object() {
      let v8_string = self.input.to_string(self.scope).unwrap();
      let string = to_utf8(v8_string, self.scope);
      visitor.visit_string(string)
    } else {
      Err(Error::ExpectedString(self.input.type_repr()))
    }
  }

  fn deserialize_option<V>(self, visitor: V) -> Result<V::Value>
  where
    V: Visitor<'de>,
  {
    if self.input.is_null_or_undefined() {
      visitor.visit_none()
    } else {
      visitor.visit_some(self)
    }
  }

  fn deserialize_unit<V>(self, visitor: V) -> Result<V::Value>
  where
    V: Visitor<'de>,
  {
    visitor.visit_unit()
  }

  fn deserialize_unit_struct<V>(
    self,
    _name: &'static str,
    visitor: V,
  ) -> Result<V::Value>
  where
    V: Visitor<'de>,
  {
    self.deserialize_unit(visitor)
  }

  // As is done here, serializers are encouraged to treat newtype structs as
  // insignificant wrappers around the data they contain. That means not
  // parsing anything other than the contained value.
  fn deserialize_newtype_struct<V>(
    self,
    _name: &'static str,
    visitor: V,
  ) -> Result<V::Value>
  where
    V: Visitor<'de>,
  {
    visitor.visit_newtype_struct(self)
  }

  fn deserialize_seq<V>(self, visitor: V) -> Result<V::Value>
  where
    V: Visitor<'de>,
  {
    let arr = v8::Local::<v8::Array>::try_from(self.input)
      .map_err(|_| Error::ExpectedArray(self.input.type_repr()))?;
    visitor.visit_seq(SeqAccess::new(arr.into(), self.scope, 0..arr.length()))
  }

  // Like deserialize_seq except it prefers tuple's length over input array's length
  fn deserialize_tuple<V>(self, len: usize, visitor: V) -> Result<V::Value>
  where
    V: Visitor<'de>,
  {
    let obj = v8::Local::<v8::Object>::try_from(self.input).unwrap();
    if obj.is_array() {
      // If the obj is an array fail if it's length differs from the tuple length
      let array = v8::Local::<v8::Array>::try_from(self.input).unwrap();
      let array_len = array.length() as usize;
      if array_len != len {
        return Err(Error::LengthMismatch(array_len, len));
      }
    }
    visitor.visit_seq(SeqAccess::new(obj, self.scope, 0..len as u32))
  }

  // Tuple structs look just like sequences in JSON.
  fn deserialize_tuple_struct<V>(
    self,
    _name: &'static str,
    len: usize,
    visitor: V,
  ) -> Result<V::Value>
  where
    V: Visitor<'de>,
  {
    self.deserialize_tuple(len, visitor)
  }

  fn deserialize_map<V>(self, visitor: V) -> Result<V::Value>
  where
    V: de::Visitor<'de>,
  {
    // Assume object, then get_own_property_names
    let obj = v8::Local::<v8::Object>::try_from(self.input)
      .map_err(|_| Error::ExpectedObject(self.input.type_repr()))?;

    if v8::Local::<v8::Map>::try_from(self.input).is_ok() {
      let pairs_array = v8::Local::<v8::Map>::try_from(self.input)
        .unwrap()
        .as_array(self.scope);
      let map = MapPairsAccess {
        pos: 0,
        len: pairs_array.length(),
        obj: pairs_array,
        scope: self.scope,
      };
      visitor.visit_map(map)
    } else {
      visitor.visit_map(MapObjectAccess::new(obj, self.scope))
    }
  }

  fn deserialize_struct<V>(
    self,
    name: &'static str,
    fields: &'static [&'static str],
    visitor: V,
  ) -> Result<V::Value>
  where
    V: Visitor<'de>,
  {
    match name {
      JsBuffer::MAGIC_NAME => {
        visit_magic(visitor, JsBuffer::from_v8(self.scope, self.input)?)
      }
      DetachedBuffer::MAGIC_NAME => {
        visit_magic(visitor, DetachedBuffer::from_v8(self.scope, self.input)?)
      }
      ByteString::MAGIC_NAME => {
        visit_magic(visitor, ByteString::from_v8(self.scope, self.input)?)
      }
      U16String::MAGIC_NAME => {
        visit_magic(visitor, U16String::from_v8(self.scope, self.input)?)
      }
      StringOrBuffer::MAGIC_NAME => {
        visit_magic(visitor, StringOrBuffer::from_v8(self.scope, self.input)?)
      }
      BigInt::MAGIC_NAME => {
        visit_magic(visitor, BigInt::from_v8(self.scope, self.input)?)
      }
      magic::Value::MAGIC_NAME => {
        visit_magic(visitor, magic::Value::from_v8(self.scope, self.input)?)
      }
      AnyValue::MAGIC_NAME => {
        visit_magic(visitor, AnyValue::from_v8(self.scope, self.input)?)
      }
      _ => {
        // Regular struct
        let obj = v8::Local::<v8::Object>::try_from(self.input)
          .map_err(|_| Error::ExpectedObject(self.input.type_repr()))?;

        // Fields names are a hint and must be inferred when not provided
        if fields.is_empty() {
          visitor.visit_map(MapObjectAccess::new(obj, self.scope))
        } else {
          visitor.visit_map(StructAccess {
            obj,
            scope: self.scope,
            keys: fields.iter(),
            next_value: None,
          })
        }
      }
    }
  }

  /// To be compatible with `serde-json`, we expect enums to be:
  /// - `"Variant"`: strings for unit variants, i.e: Enum::Variant
  /// - `{ Variant: payload }`: single K/V pairs, converted to `Enum::Variant { payload }`
  fn deserialize_enum<V>(
    self,
    _name: &str,
    _variants: &'static [&'static str],
    visitor: V,
  ) -> Result<V::Value>
  where
    V: Visitor<'de>,
  {
    // Unit variant
    if self.input.is_string() || self.input.is_string_object() {
      let payload = v8::undefined(self.scope).into();
      visitor.visit_enum(EnumAccess {
        scope: self.scope,
        tag: self.input,
        payload,
      })
    }
    // Struct or tuple variant
    else if self.input.is_object() {
      // Assume object
      let obj = v8::Local::<v8::Object>::try_from(self.input).unwrap();
      // Unpack single-key
      let tag = {
        let prop_names =
          obj.get_own_property_names(self.scope, Default::default());
        let prop_names = prop_names
          .ok_or_else(|| Error::ExpectedEnum(self.input.type_repr()))?;
        let prop_names_len = prop_names.length();
        if prop_names_len != 1 {
          return Err(Error::LengthMismatch(prop_names_len as usize, 1));
        }
        prop_names.get_index(self.scope, 0).unwrap()
      };

      let payload = obj.get(self.scope, tag).unwrap();
      visitor.visit_enum(EnumAccess {
        scope: self.scope,
        tag,
        payload,
      })
    } else {
      Err(Error::ExpectedEnum(self.input.type_repr()))
    }
  }

  // An identifier in Serde is the type that identifies a field of a struct or
  // the variant of an enum. In JSON, struct fields and enum variants are
  // represented as strings. In other formats they may be represented as
  // numeric indices.
  fn deserialize_identifier<V>(self, visitor: V) -> Result<V::Value>
  where
    V: Visitor<'de>,
  {
    self.deserialize_str(visitor)
  }

  fn deserialize_ignored_any<V>(self, visitor: V) -> Result<V::Value>
  where
    V: Visitor<'de>,
  {
    visitor.visit_none()
  }

  fn deserialize_bytes<V>(self, visitor: V) -> Result<V::Value>
  where
    V: Visitor<'de>,
  {
    magic::buffer::JsBuffer::from_v8(self.scope, self.input)
      .and_then(|zb| visitor.visit_bytes(&zb))
  }

  fn deserialize_byte_buf<V>(self, visitor: V) -> Result<V::Value>
  where
    V: Visitor<'de>,
  {
    magic::buffer::JsBuffer::from_v8(self.scope, self.input)
      .and_then(|zb| visitor.visit_byte_buf(Vec::from(&*zb)))
  }
}

struct MapObjectAccess<'a, 's> {
  obj: v8::Local<'a, v8::Object>,
  keys: SeqAccess<'a, 's>,
  next_value: Option<v8::Local<'s, v8::Value>>,
}

impl<'a, 's> MapObjectAccess<'a, 's> {
  pub fn new(
    obj: v8::Local<'a, v8::Object>,
    scope: &'a mut v8::HandleScope<'s>,
  ) -> Self {
    let keys = match obj.get_own_property_names(
      scope,
      v8::GetPropertyNamesArgsBuilder::new()
        .key_conversion(v8::KeyConversionMode::ConvertToString)
        .build(),
    ) {
      Some(keys) => SeqAccess::new(keys.into(), scope, 0..keys.length()),
      None => SeqAccess::new(obj, scope, 0..0),
    };

    Self {
      obj,
      keys,
      next_value: None,
    }
  }
}

impl<'de> de::MapAccess<'de> for MapObjectAccess<'_, '_> {
  type Error = Error;

  fn next_key_seed<K: de::DeserializeSeed<'de>>(
    &mut self,
    seed: K,
  ) -> Result<Option<K::Value>> {
    while let Some(key) = self.keys.next_element::<magic::Value>()? {
      let v8_val = self.obj.get(self.keys.scope, key.v8_value).unwrap();
      if v8_val.is_undefined() {
        // Historically keys/value pairs with undefined values are not added to the output
        continue;
      }
      self.next_value = Some(v8_val);
      let mut deserializer =
        Deserializer::new(self.keys.scope, key.v8_value, None);
      return seed.deserialize(&mut deserializer).map(Some);
    }
    Ok(None)
  }

  fn next_value_seed<V: de::DeserializeSeed<'de>>(
    &mut self,
    seed: V,
  ) -> Result<V::Value> {
    let v8_val = self
      .next_value
      .take()
      .expect("Call next_key_seed before next_value_seed");
    let mut deserializer = Deserializer::new(self.keys.scope, v8_val, None);
    seed.deserialize(&mut deserializer)
  }

  fn size_hint(&self) -> Option<usize> {
    self.keys.size_hint()
  }
}

struct MapPairsAccess<'a, 's> {
  obj: v8::Local<'a, v8::Array>,
  pos: u32,
  len: u32,
  scope: &'a mut v8::HandleScope<'s>,
}

impl<'de> de::MapAccess<'de> for MapPairsAccess<'_, '_> {
  type Error = Error;

  fn next_key_seed<K: de::DeserializeSeed<'de>>(
    &mut self,
    seed: K,
  ) -> Result<Option<K::Value>> {
    if self.pos < self.len {
      let v8_key = self.obj.get_index(self.scope, self.pos).unwrap();
      self.pos += 1;
      let mut deserializer = Deserializer::new(self.scope, v8_key, None);
      let k = seed.deserialize(&mut deserializer)?;
      Ok(Some(k))
    } else {
      Ok(None)
    }
  }

  fn next_value_seed<V: de::DeserializeSeed<'de>>(
    &mut self,
    seed: V,
  ) -> Result<V::Value> {
    debug_assert!(self.pos < self.len);
    let v8_val = self.obj.get_index(self.scope, self.pos).unwrap();
    self.pos += 1;
    let mut deserializer = Deserializer::new(self.scope, v8_val, None);
    seed.deserialize(&mut deserializer)
  }

  fn size_hint(&self) -> Option<usize> {
    Some((self.len - self.pos) as usize / 2)
  }
}

struct StructAccess<'a, 's> {
  obj: v8::Local<'a, v8::Object>,
  scope: &'a mut v8::HandleScope<'s>,
  keys: std::slice::Iter<'static, &'static str>,
  next_value: Option<v8::Local<'s, v8::Value>>,
}

impl<'de> de::MapAccess<'de> for StructAccess<'_, '_> {
  type Error = Error;

  fn next_key_seed<K>(&mut self, seed: K) -> Result<Option<K::Value>>
  where
    K: de::DeserializeSeed<'de>,
  {
    for field in self.keys.by_ref() {
      let key = v8_struct_key(self.scope, field).into();
      let val = self.obj.get(self.scope, key).unwrap();
      if val.is_undefined() {
        // Historically keys/value pairs with undefined values are not added to the output
        continue;
      }
      self.next_value = Some(val);
      let mut deserializer = Deserializer::new(self.scope, key, None);
      return seed.deserialize(&mut deserializer).map(Some);
    }
    Ok(None)
  }

  fn next_value_seed<V>(&mut self, seed: V) -> Result<V::Value>
  where
    V: de::DeserializeSeed<'de>,
  {
    let val = self
      .next_value
      .take()
      .expect("Call next_key_seed before next_value_seed");
    let mut deserializer = Deserializer::new(self.scope, val, None);
    seed.deserialize(&mut deserializer)
  }
}

struct SeqAccess<'a, 's> {
  obj: v8::Local<'a, v8::Object>,
  scope: &'a mut v8::HandleScope<'s>,
  range: std::ops::Range<u32>,
}

impl<'a, 's> SeqAccess<'a, 's> {
  pub fn new(
    obj: v8::Local<'a, v8::Object>,
    scope: &'a mut v8::HandleScope<'s>,
    range: std::ops::Range<u32>,
  ) -> Self {
    Self { obj, scope, range }
  }
}

impl<'de> de::SeqAccess<'de> for SeqAccess<'_, '_> {
  type Error = Error;

  fn next_element_seed<T: de::DeserializeSeed<'de>>(
    &mut self,
    seed: T,
  ) -> Result<Option<T::Value>> {
    if let Some(pos) = self.range.next() {
      let val = self.obj.get_index(self.scope, pos).unwrap();
      let mut deserializer = Deserializer::new(self.scope, val, None);
      seed.deserialize(&mut deserializer).map(Some)
    } else {
      Ok(None)
    }
  }

  fn size_hint(&self) -> Option<usize> {
    self.range.size_hint().1
  }
}

struct EnumAccess<'a, 'b, 's> {
  tag: v8::Local<'a, v8::Value>,
  payload: v8::Local<'a, v8::Value>,
  scope: &'b mut v8::HandleScope<'s>,
  // p1: std::marker::PhantomData<&'x ()>,
}

impl<'de, 'a, 'b, 's> de::EnumAccess<'de> for EnumAccess<'a, 'b, 's> {
  type Error = Error;
  type Variant = VariantDeserializer<'a, 'b, 's>;

  fn variant_seed<V: de::DeserializeSeed<'de>>(
    self,
    seed: V,
  ) -> Result<(V::Value, Self::Variant)> {
    let seed = {
      let mut dtag = Deserializer::new(self.scope, self.tag, None);
      seed.deserialize(&mut dtag)
    };
    let dpayload = VariantDeserializer::<'a, 'b, 's> {
      scope: self.scope,
      value: self.payload,
    };

    Ok((seed?, dpayload))
  }
}

struct VariantDeserializer<'a, 'b, 's> {
  value: v8::Local<'a, v8::Value>,
  scope: &'b mut v8::HandleScope<'s>,
}

impl<'de, 'a, 'b, 's> de::VariantAccess<'de>
  for VariantDeserializer<'a, 'b, 's>
{
  type Error = Error;

  fn unit_variant(self) -> Result<()> {
    let mut d = Deserializer::new(self.scope, self.value, None);
    de::Deserialize::deserialize(&mut d)
  }

  fn newtype_variant_seed<T: de::DeserializeSeed<'de>>(
    self,
    seed: T,
  ) -> Result<T::Value> {
    let mut d = Deserializer::new(self.scope, self.value, None);
    seed.deserialize(&mut d)
  }

  fn tuple_variant<V: de::Visitor<'de>>(
    self,
    len: usize,
    visitor: V,
  ) -> Result<V::Value> {
    let mut d = Deserializer::new(self.scope, self.value, None);
    de::Deserializer::deserialize_tuple(&mut d, len, visitor)
  }

  fn struct_variant<V: de::Visitor<'de>>(
    self,
    fields: &'static [&'static str],
    visitor: V,
  ) -> Result<V::Value> {
    let mut d = Deserializer::new(self.scope, self.value, None);
    de::Deserializer::deserialize_struct(&mut d, "", fields, visitor)
  }
}

fn bigint_to_f64(b: v8::Local<v8::BigInt>) -> f64 {
  // log2(f64::MAX) == log2(1.7976931348623157e+308) == 1024
  let mut words: [u64; 16] = [0; 16]; // 1024/64 => 16 64bit words
  let (neg, words) = b.to_words_array(&mut words);
  if b.word_count() > 16 {
    return match neg {
      true => f64::NEG_INFINITY,
      false => f64::INFINITY,
    };
  }
  let sign = if neg { -1.0 } else { 1.0 };
  let x: f64 = words
    .iter()
    .enumerate()
    .map(|(i, w)| (*w as f64) * 2.0f64.powi(64 * i as i32))
    .sum();
  sign * x
}

pub fn to_utf8(
  s: v8::Local<v8::String>,
  scope: &mut v8::HandleScope,
) -> String {
  to_utf8_fast(s, scope).unwrap_or_else(|| to_utf8_slow(s, scope))
}

fn to_utf8_fast(
  s: v8::Local<v8::String>,
  scope: &mut v8::HandleScope,
) -> Option<String> {
  // Over-allocate by 20% to avoid checking string twice
  let str_chars = s.length();
  let capacity = (str_chars as f64 * 1.2) as usize;
  let mut buf = Vec::with_capacity(capacity);

  let mut nchars = 0;
  let bytes_len = s.write_utf8_uninit(
    scope,
    buf.spare_capacity_mut(),
    Some(&mut nchars),
    v8::WriteOptions::NO_NULL_TERMINATION
      | v8::WriteOptions::REPLACE_INVALID_UTF8,
  );

  if nchars < str_chars {
    return None;
  }

  // SAFETY: write_utf8_uninit guarantees `bytes_len` bytes are initialized & valid utf8
  unsafe {
    buf.set_len(bytes_len);
    Some(String::from_utf8_unchecked(buf))
  }
}

fn to_utf8_slow(
  s: v8::Local<v8::String>,
  scope: &mut v8::HandleScope,
) -> String {
  let capacity = s.utf8_length(scope);
  let mut buf = Vec::with_capacity(capacity);

  let bytes_len = s.write_utf8_uninit(
    scope,
    buf.spare_capacity_mut(),
    None,
    v8::WriteOptions::NO_NULL_TERMINATION
      | v8::WriteOptions::REPLACE_INVALID_UTF8,
  );

  // SAFETY: write_utf8_uninit guarantees `bytes_len` bytes are initialized & valid utf8
  unsafe {
    buf.set_len(bytes_len);
    String::from_utf8_unchecked(buf)
  }
}
