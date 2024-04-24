// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

pub trait ToV8<'a> {
  type Error: std::error::Error;

  fn to_v8(
    &mut self,
    scope: &mut v8::HandleScope<'a>,
  ) -> Result<v8::Local<'a, v8::Value>, Self::Error>;
}

pub trait FromV8<'a>: Sized {
  type Error: std::error::Error;

  fn from_v8(
    scope: &mut v8::HandleScope<'a>,
    value: v8::Local<'a, v8::Value>,
  ) -> Result<Self, Self::Error>;
}

// Impls for Option

impl<'a, T> ToV8<'a> for Option<T>
where
  T: ToV8<'a>,
{
  type Error = T::Error;

  fn to_v8(
    &mut self,
    scope: &mut v8::HandleScope<'a>,
  ) -> Result<v8::Local<'a, v8::Value>, Self::Error> {
    match self {
      Some(x) => x.to_v8(scope).map(Into::into),
      None => Ok(v8::undefined(scope).into()),
    }
  }
}

impl<'a, T> FromV8<'a> for Option<T>
where
  T: FromV8<'a>,
{
  type Error = T::Error;

  fn from_v8(
    scope: &mut v8::HandleScope<'a>,
    value: v8::Local<'a, v8::Value>,
  ) -> Result<Self, Self::Error> {
    if value.is_null_or_undefined() {
      Ok(None)
    } else {
      T::from_v8(scope, value).map(Some)
    }
  }
}
