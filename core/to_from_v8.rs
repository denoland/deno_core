// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

use crate::{error::StdAnyError, runtime::ops};
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
  const NAME: &'static str;
  fn as_i32(self) -> i32;
  fn from_i32(value: i32) -> Self;
}

macro_rules! impl_smallint {
  (for $($t:ty),*) => {
    $(
      impl SmallInt for $t {
        const NAME: &'static str = stringify!($t);
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

  #[inline]
  fn to_v8(
    self,
    scope: &mut v8::HandleScope<'a>,
  ) -> Result<v8::Local<'a, v8::Value>, Self::Error> {
    Ok(v8::Integer::new(scope, self.0.as_i32()).into())
  }
}

impl<'a, T: SmallInt> FromV8<'a> for Smi<T> {
  type Error = StdAnyError;

  #[inline]
  fn from_v8(
    _scope: &mut v8::HandleScope<'a>,
    value: v8::Local<'a, v8::Value>,
  ) -> Result<Self, Self::Error> {
    let v = crate::runtime::ops::to_i32_option(&value)
      .ok_or_else(|| crate::error::type_error(format!("{}", T::NAME)))?;
    Ok(Smi(T::from_i32(v)))
  }
}

pub struct Number<T: Numeric>(pub T);

pub trait Numeric: Sized {
  const NAME: &'static str;
  fn from_value(value: &v8::Value) -> Option<Self>;
  fn as_f64(self) -> f64;
}

macro_rules! impl_numeric {
  ($($t:ty : $from: path ),*) => {
    $(
      impl Numeric for $t {
        const NAME: &'static str = stringify!($t);
        #[inline(always)]
        fn from_value(value: &v8::Value) -> Option<Self> {
          $from(value).map(|v| v as _)
        }

        #[inline(always)]
        fn as_f64(self) -> f64 {
            self as _
        }
      }
    )*
  };
}

impl_numeric!(
  f32   : ops::to_f32_option,
  f64   : ops::to_f64_option,
  u32   : ops::to_u32_option,
  u64   : ops::to_u64_option,
  usize : ops::to_u64_option,
  i32   : ops::to_i32_option,
  i64   : ops::to_i64_option,
  isize : ops::to_i64_option
);

impl<'a, T: Numeric> ToV8<'a> for Number<T> {
  type Error = Infallible;
  #[inline]
  fn to_v8(
    self,
    scope: &mut v8::HandleScope<'a>,
  ) -> Result<v8::Local<'a, v8::Value>, Self::Error> {
    Ok(v8::Number::new(scope, self.0.as_f64()).into())
  }
}

impl<'a, T: Numeric> FromV8<'a> for Number<T> {
  type Error = StdAnyError;
  #[inline]
  fn from_v8(
    _scope: &mut v8::HandleScope<'a>,
    value: v8::Local<'a, v8::Value>,
  ) -> Result<Self, Self::Error> {
    T::from_value(&value).map(Number).ok_or_else(|| {
      crate::error::type_error(format!("Expected {}", T::NAME)).into()
    })
  }
}

impl<'a> ToV8<'a> for bool {
  type Error = Infallible;
  #[inline]
  fn to_v8(
    self,
    scope: &mut v8::HandleScope<'a>,
  ) -> Result<v8::Local<'a, v8::Value>, Self::Error> {
    Ok(v8::Boolean::new(scope, self).into())
  }
}

impl<'a> FromV8<'a> for bool {
  type Error = Infallible;
  #[inline]
  fn from_v8(
    _scope: &mut v8::HandleScope<'a>,
    value: v8::Local<'a, v8::Value>,
  ) -> Result<Self, Self::Error> {
    Ok(value.is_true())
  }
}
