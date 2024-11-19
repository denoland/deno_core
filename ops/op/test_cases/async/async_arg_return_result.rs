// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();

#[op(async)]
pub async fn op_async(x: i32) -> std::io::Result<i32> {
  Ok(x)
}
