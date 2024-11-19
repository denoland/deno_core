// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();

use deno_core::v8;

#[op2]
fn op_v8_string<'a>(
  _str1: &v8::String,
  _str2: v8::Local<v8::String>,
) -> v8::Local<'a, v8::String> {
  unimplemented!()
}
