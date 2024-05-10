// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

use std::ops::Deref;
use std::ops::DerefMut;

use crate::Error;

use super::transl8::impl_magic;
use super::transl8::FromV8;
use super::transl8::ToV8;

#[derive(PartialEq, Eq, Clone, Debug, Default)]
pub struct U16String(Vec<u16>);
impl_magic!(U16String);

impl Deref for U16String {
  type Target = Vec<u16>;

  fn deref(&self) -> &Self::Target {
    &self.0
  }
}

impl DerefMut for U16String {
  fn deref_mut(&mut self) -> &mut Self::Target {
    &mut self.0
  }
}

impl AsRef<[u16]> for U16String {
  fn as_ref(&self) -> &[u16] {
    &self.0
  }
}

impl AsMut<[u16]> for U16String {
  fn as_mut(&mut self) -> &mut [u16] {
    &mut self.0
  }
}

impl<const N: usize> From<[u16; N]> for U16String {
  fn from(value: [u16; N]) -> Self {
    Self(value.into())
  }
}

impl<const N: usize> From<&[u16; N]> for U16String {
  fn from(value: &[u16; N]) -> Self {
    Self(value.into())
  }
}

impl From<&[u16]> for U16String {
  fn from(value: &[u16]) -> Self {
    Self(value.into())
  }
}

impl From<Vec<u16>> for U16String {
  fn from(value: Vec<u16>) -> Self {
    Self(value)
  }
}

impl ToV8 for U16String {
  fn to_v8<'a>(
    &self,
    scope: &mut v8::HandleScope<'a>,
  ) -> Result<v8::Local<'a, v8::Value>, crate::Error> {
    let maybe_v =
      v8::String::new_from_two_byte(scope, self, v8::NewStringType::Normal);

    // 'new_from_two_byte' can return 'None' if buffer length > kMaxLength.
    if let Some(v) = maybe_v {
      Ok(v.into())
    } else {
      Err(Error::Message(String::from(
        "Cannot allocate String from UTF-16: buffer exceeds maximum length.",
      )))
    }
  }
}

impl FromV8 for U16String {
  fn from_v8(
    scope: &mut v8::HandleScope,
    value: v8::Local<v8::Value>,
  ) -> Result<Self, crate::Error> {
    let v8str = v8::Local::<v8::String>::try_from(value)
      .map_err(|_| Error::ExpectedString(value.type_repr()))?;
    let len = v8str.length();
    let mut buffer = Vec::with_capacity(len);
    #[allow(clippy::uninit_vec)]
    // SAFETY: we set length == capacity (see previous line),
    // before immediately writing into that buffer and sanity check with an assert
    unsafe {
      buffer.set_len(len);
      let written = v8str.write(
        scope,
        &mut buffer,
        0,
        v8::WriteOptions::NO_NULL_TERMINATION,
      );
      assert!(written == len);
    }
    Ok(buffer.into())
  }
}
