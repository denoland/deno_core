// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();

use deno_core::error::AnyError;

#[op(fast)]
pub fn op_u32_with_result() -> Result<u32, AnyError> {
  Ok(0)
}
