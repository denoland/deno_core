// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use deno_core::op2;
use deno_core::external;
use deno_core::ExternalPointer;

#[op2(async)]
pub async fn op_async_void_deferred() {
  tokio::task::yield_now().await
}

struct Barrier(tokio::sync::Barrier);

external!(Barrier, "barrier");

#[op2(fast)]
pub fn op_async_barrier_create(count: u32) -> *const std::ffi::c_void {
  let barrier = Barrier(tokio::sync::Barrier::new(count as _));
  ExternalPointer::new(barrier).into_raw()
}

#[op2(async)]
pub async fn op_async_barrier_await(barrier: *const std::ffi::c_void) {
  let barrier = ExternalPointer::<Barrier>::from_raw(barrier);
  unsafe { barrier.unsafely_deref().0.wait().await; }
}

#[op2(fast)]
pub fn op_async_barrier_destroy(barrier: *const std::ffi::c_void) {
  let barrier = ExternalPointer::<Barrier>::from_raw(barrier);
  unsafe { barrier.unsafely_take(); }
}
