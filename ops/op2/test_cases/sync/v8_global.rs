// Copyright 2018-2025 the Deno authors. MIT license.

#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();

use deno_core::v8;

#[op2]
#[global]
pub fn op_global(
  #[global] g: v8::Global<v8::String>,
) -> v8::Global<v8::String> {
  g
}
