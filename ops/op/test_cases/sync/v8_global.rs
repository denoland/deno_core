// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();

use deno_core::v8;

#[op]
#[global]
pub fn op_global(
  #[global] g: v8::Global<v8::String>,
) -> v8::Global<v8::String> {
  g
}
