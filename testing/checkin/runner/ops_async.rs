// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
use deno_core::op2;
use deno_core::OpState;
use futures::future::poll_fn;
use std::cell::RefCell;
use std::future::Future;
use std::rc::Rc;

use super::testing::Output;
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
pub async fn op_async_spin_on_state(state: Rc<RefCell<OpState>>) {
  poll_fn(|cx| {
    // Ensure that we never get polled when the state has been emptied
    state.borrow().borrow::<Output>();
    cx.waker().wake_by_ref();
    std::task::Poll::Pending
  })
  .await
}
