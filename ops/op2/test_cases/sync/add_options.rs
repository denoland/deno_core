// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();

#[op2]
pub fn op_test_add_option(a: u32, b: Option<u32>) -> u32 {
  a + b.unwrap_or(100)
}
