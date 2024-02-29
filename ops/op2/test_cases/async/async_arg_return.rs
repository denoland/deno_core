// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();

#[op2(async)]
pub async fn op_async(x: i32) -> i32 {
  x
}
