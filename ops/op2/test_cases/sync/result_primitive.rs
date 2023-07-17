// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
deno_ops_compile_test_runner::prelude!();

use deno_core::error::AnyError;

#[op2(fast)]
pub fn op_u32_with_result() -> Result<u32, AnyError> {
    Ok(0)
}
