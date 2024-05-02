// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

use std::convert::Infallible;

pub trait ToV8<'a> {
  type Error: std::error::Error + Send + Sync + 'static;

  fn to_v8(
    self,
    scope: &mut v8::HandleScope<'a>,
  ) -> Result<v8::Local<'a, v8::Value>, Self::Error>;
}

pub trait FromV8<'a>: Sized {
  type Error: std::error::Error + Send + Sync + 'static;

  fn from_v8(
    scope: &mut v8::HandleScope<'a>,
    value: v8::Local<'a, v8::Value>,
  ) -> Result<Self, Self::Error>;
}

// impls

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct Smi<T: SmallInt>(pub T);

pub trait SmallInt {
  fn as_i32(self) -> i32;
  fn from_i32(value: i32) -> Self;
}

macro_rules! impl_smallint {
    (for $($t:ty),*) => {
        $(
            impl SmallInt for $t {
                #[inline(always)]
                fn as_i32(self) -> i32 {
                  self as _
                }

                #[inline(always)]
                fn from_i32(value: i32) -> Self {
                    value as _
                }
            }
        )*
    };
}

impl_smallint!(for u8, u16, u32, u64, usize, i8, i16, i32, i64, isize);

impl<'a, T: SmallInt> ToV8<'a> for Smi<T> {
  type Error = Infallible;

  fn to_v8(
    self,
    scope: &mut v8::HandleScope<'a>,
  ) -> Result<v8::Local<'a, v8::Value>, Self::Error> {
    Ok(v8::Integer::new(scope, self.0.as_i32()).into())
  }
}

impl<'a, T: SmallInt> FromV8<'a> for Smi<T> {
  type Error = Infallible;

  fn from_v8(
    _scope: &mut v8::HandleScope<'a>,
    value: v8::Local<'a, v8::Value>,
  ) -> Result<Self, Self::Error> {
    let v = crate::runtime::ops::to_i32_option(&value).unwrap_or_default();
    Ok(Smi(T::from_i32(v)))
  }
}

