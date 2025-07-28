// Copyright 2018-2025 the Deno authors. MIT license.

#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();
use deno_core::FromV8;
use deno_core::v8;

struct Foo;

impl<'a> FromV8<'a> for Foo {
  type Error = std::convert::Infallible;
  fn from_v8(
    _scope: &mut v8::HandleScope<'a>,
    _value: v8::Local<'a, v8::Value>,
  ) -> Result<Self, Self::Error> {
    Ok(Foo)
  }
}

#[op2]
#[from_v8]
pub fn op_from_v8_arg(#[from_v8] foo: Foo) {
  let _ = foo;
}
