// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();

use deno_core::error::AnyError;

#[op2(fast)]
pub fn op_bool(arg: bool) -> Result<bool, AnyError> {
  Ok(arg)
}
