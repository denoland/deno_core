// Copyright 2018-2025 the Deno authors. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();

use deno_core::error::AnyError;
use std::future::Future;

#[op2(async)]
pub fn op_async_result_impl(
  x: i32,
) -> Result<impl Future<Output = std::io::Result<i32>>, AnyError> {
  Ok(async move { Ok(x) })
}
