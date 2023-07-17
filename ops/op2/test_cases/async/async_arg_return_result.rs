// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
deno_ops_compile_test_runner::prelude!();

#[op2(async)]
pub async fn op_async(x: i32) -> std::io::Result<i32> {
    Ok(0)
}
