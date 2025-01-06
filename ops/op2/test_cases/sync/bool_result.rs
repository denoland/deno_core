// Copyright 2018-2025 the Deno authors. MIT license.

#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();

use deno_core::error::OpError;

#[op2(fast)]
pub fn op_bool(arg: bool) -> Result<bool, OpError> {
  Ok(arg)
}
