// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
deno_ops_compile_test_runner::prelude!();

use deno_core::error::AnyError;
use deno_core::v8;

#[op2]
pub fn op_void_with_result(scope: &mut v8::HandleScope) -> Result<(), AnyError> {
    Ok(())
}
