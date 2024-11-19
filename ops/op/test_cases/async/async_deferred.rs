// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();

#[op(async(deferred), fast)]
pub async fn op_async_deferred() -> std::io::Result<i32> {
  Ok(0)
}
