use anyhow::{bail, Error};
use deno_core::error::{type_error, JsError};
// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use deno_core::op2;
use std::future::Future;
use std::rc::Rc;

use super::testing::TestData;

#[op2(async)]
pub async fn op_async_yield() {
  tokio::task::yield_now().await
}

#[op2(fast)]
pub fn op_async_barrier_create(
  #[state] test_data: &mut TestData,
  #[string] name: String,
  count: u32,
) {
  let barrier = Rc::new(tokio::sync::Barrier::new(count as _));
  test_data.insert(name, barrier);
}

#[op2(async)]
pub fn op_async_barrier_await(
  #[state] test_data: &TestData,
  #[string] name: String,
) -> impl Future<Output = ()> {
  let barrier: &Rc<tokio::sync::Barrier> = test_data.get(name);
  let barrier = barrier.clone();
  async move {
    barrier.wait().await;
  }
}

#[op2(async)]
pub async fn op_async_throw_error_eager() -> Result<(), Error> {
  Err(type_error("Error"))
}

#[op2(async(deferred), fast)]
pub async fn op_async_throw_error_deferred() -> Result<(), Error> {
  Err(type_error("Error"))
}

#[op2(async(lazy), fast)]
pub async fn op_async_throw_error_lazy() -> Result<(), Error> {
  Err(type_error("Error"))
}
