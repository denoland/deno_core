// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();

use deno_core::error::AnyError;
use deno_core::JsBuffer;
use deno_core::OpState;
use std::cell::RefCell;
use std::future::Future;
use std::rc::Rc;

// Collect a few examples that we'll smoke test when not running on the CI.

#[op2(async)]
pub async fn op_async1() {}

#[op2(async)]
pub async fn op_async2(x: i32) -> i32 {
  x
}

#[op2(async)]
pub async fn op_async3(x: i32) -> std::io::Result<i32> {
  Ok(x)
}

#[op2(async)]
pub fn op_async4(x: i32) -> Result<impl Future<Output = i32>, AnyError> {
  Ok(async move { x })
}

#[op2(async)]
pub fn op_async5(
  x: i32,
) -> Result<impl Future<Output = std::io::Result<i32>>, AnyError> {
  Ok(async move { Ok(x) })
}

#[op2(async)]
pub async fn op_async6(x: f32) -> f32 {
  x
}

#[op2(async)]
pub async fn op_async_opstate(
  state: Rc<RefCell<OpState>>,
) -> std::io::Result<i32> {
  Ok(*state.borrow().borrow::<i32>())
}

#[op2(async)]
#[buffer]
pub async fn op_async_buffer(#[buffer] buf: JsBuffer) -> JsBuffer {
  buf
}

#[op2(async)]
#[string]
pub async fn op_async_string(#[string] s: String) -> String {
  s
}
