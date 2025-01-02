// Copyright 2018-2025 the Deno authors. MIT license.

#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();

use deno_core::error::AnyError;

#[op2(fast)]
pub fn op_external_with_result() -> Result<*mut std::ffi::c_void, AnyError> {
  Ok(0 as _)
}
