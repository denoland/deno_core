// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();

#[op2(fast)]
pub fn op_void_with_result() -> std::io::Result<()> {
    Ok(())
}
