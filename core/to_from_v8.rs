// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

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
