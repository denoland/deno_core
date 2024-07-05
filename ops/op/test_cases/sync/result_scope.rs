// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();

use deno_core::error::AnyError;
use deno_core::v8;

#[op]
pub fn op_void_with_result(
  _scope: &mut v8::HandleScope,
) -> Result<(), AnyError> {
  Ok(())
}
