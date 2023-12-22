// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use super::erased_future::ErasedFuture;
use super::op_results::*;
use super::OpDriver;
use super::RetValMapper;
use crate::arena::ArenaBox;
use crate::arena::ArenaUnique;
use crate::ops::OpCtx;
use crate::GetErrorClassFn;
use crate::OpId;
use crate::PromiseId;
use anyhow::Error;
use futures::stream::FuturesUnordered;
use futures::task::noop_waker_ref;
use futures::FutureExt;
use futures::StreamExt;
use std::cell::RefCell;
use std::cell::UnsafeCell;
use std::future::ready;
use std::future::Future;
use std::pin::Pin;
use std::task::ready;
use std::task::Context;
use std::task::Poll;
use std::task::Waker;

const MAX_ARENA_FUTURE_SIZE: usize = 1024;
const FUTURE_ARENA_COUNT: usize = 256;

enum FutureAllocation<R: 'static> {
  Arena(ArenaBox<ErasedFuture<MAX_ARENA_FUTURE_SIZE, R>>),
  Box(Pin<Box<dyn Future<Output = R>>>),
}

impl<R> Unpin for FutureAllocation<R> {}

impl<R> Future for FutureAllocation<R> {
  type Output = R;

  #[inline(always)]
  fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
    match self.get_mut() {
      Self::Arena(f) => f.poll_unpin(cx),
      Self::Box(f) => f.poll_unpin(cx),
    }
  }
}

/// [`OpDriver`] implementation built on a tokio [`FuturesUnordered`].
pub struct FuturesUnorderedDriver {
  pending_ops:
    RefCell<FuturesUnordered<ErasedFuture<MAX_ARENA_FUTURE_SIZE, PendingOp>>>,
  arena: ArenaUnique<ErasedFuture<MAX_ARENA_FUTURE_SIZE, ()>>,
  waker: UnsafeCell<Option<Waker>>,
}

impl Default for FuturesUnorderedDriver {
  fn default() -> Self {
    Self {
      pending_ops: Default::default(),
      arena: ArenaUnique::with_capacity(FUTURE_ARENA_COUNT),
      waker: UnsafeCell::default(),
    }
  }
}

impl FuturesUnorderedDriver {
  /// Allocate a future to run in this `FuturesUnordered`. If the future is too large, or the arena
  /// is full, allocated in the heap.
  fn allocate<F, R>(&self, f: F) -> FutureAllocation<R>
  where
    F: Future<Output = R> + 'static,
  {
    if std::mem::size_of::<F>() > MAX_ARENA_FUTURE_SIZE {
      FutureAllocation::Box(f.boxed_local())
    } else {
      unsafe {
        match self.arena.reserve_space() {
          Some(reservation) => {
            let alloc = self.arena.complete_reservation(
              reservation,
              std::mem::transmute(
                ErasedFuture::<MAX_ARENA_FUTURE_SIZE, _>::new(f),
              ),
            );
            FutureAllocation::Arena(std::mem::transmute(alloc))
          }
          None => FutureAllocation::Box(f.boxed_local()),
        }
      }
    }
  }

  #[inline(always)]
  fn wake(&self) {
    unsafe {
      if let Some(waker) = (*self.waker.get()).as_ref() {
        waker.wake_by_ref();
      }
    }
  }

  /// Spawn an unpolled task, along with a function that can map it to a [`PendingOp`].
  #[inline(always)]
  fn spawn_unpolled<R>(
    &self,
    task: impl Future<Output = R> + 'static,
    map: impl FnOnce(R) -> PendingOp + 'static,
  ) {
    self
      .pending_ops
      .borrow_mut()
      .push(ErasedFuture::new(task.map(map)));
    self.wake();
  }

  /// Spawn a ready task that already has a [`PendingOp`].
  #[inline(always)]
  fn spawn_ready(&self, ready_op: PendingOp) {
    self
      .pending_ops
      .borrow_mut()
      .push(ErasedFuture::new(ready(ready_op)));
    self.wake();
  }

  /// Spawn a polled task inside a [`FutureAllocation`], along with a function that can map it to a [`PendingOp`].
  #[inline(always)]
  fn spawn_polled<R>(
    &self,
    task: FutureAllocation<R>,
    map: impl FnOnce(R) -> PendingOp + 'static,
  ) {
    self
      .pending_ops
      .borrow_mut()
      .push(ErasedFuture::new(task.map(map)));
    self.wake();
  }

  #[inline(always)]
  fn pending_op_success<R: 'static>(
    info: PendingOpInfo,
    rv_map: RetValMapper<R>,
    v: R,
  ) -> PendingOp {
    PendingOp(info, OpResult::new_value(v, rv_map))
  }

  #[inline(always)]
  fn pending_op_failure<E: Into<Error> + 'static>(
    info: PendingOpInfo,
    get_class: GetErrorClassFn,
    err: E,
  ) -> PendingOp {
    PendingOp(info, OpResult::Err(OpError::new(get_class, err.into())))
  }

  #[inline(always)]
  fn pending_op_result<R: 'static, E: Into<Error> + 'static>(
    info: PendingOpInfo,
    rv_map: RetValMapper<R>,
    get_class: GetErrorClassFn,
    result: Result<R, E>,
  ) -> PendingOp {
    match result {
      Ok(r) => Self::pending_op_success(info, rv_map, r),
      Err(err) => Self::pending_op_failure(info, get_class, err),
    }
  }
}

impl OpDriver for FuturesUnorderedDriver {
  fn submit_op_fallible<
    R: 'static,
    E: Into<Error> + 'static,
    const LAZY: bool,
    const DEFERRED: bool,
  >(
    &self,
    ctx: &OpCtx,
    promise_id: i32,
    op: impl Future<Output = Result<R, E>> + 'static,
    rv_map: RetValMapper<R>,
  ) -> Option<Result<R, E>> {
    {
      let info = PendingOpInfo(promise_id, ctx.id, ctx.metrics_enabled());
      let get_class = ctx.get_error_class_fn;

      if LAZY {
        self.spawn_unpolled(op, move |r| {
          Self::pending_op_result(info, rv_map, get_class, r)
        });
        return None;
      }

      // We poll every future here because it's much faster to return a result than
      // spin the event loop to get it.
      let mut pinned = self.allocate(op);
      match pinned.poll_unpin(&mut Context::from_waker(noop_waker_ref())) {
        Poll::Pending => self.spawn_polled(pinned, move |r| {
          Self::pending_op_result(info, rv_map, get_class, r)
        }),
        Poll::Ready(res) => {
          if DEFERRED {
            self.spawn_ready(Self::pending_op_result(
              info, rv_map, get_class, res,
            ))
          } else {
            return Some(res);
          }
        }
      };
      None
    }
  }

  fn submit_op_infallible<
    R: 'static,
    const LAZY: bool,
    const DEFERRED: bool,
  >(
    &self,
    ctx: &OpCtx,
    promise_id: i32,
    op: impl Future<Output = R> + 'static,
    rv_map: RetValMapper<R>,
  ) -> Option<R> {
    {
      let info = PendingOpInfo(promise_id, ctx.id, ctx.metrics_enabled());
      if LAZY {
        self.spawn_unpolled(op, move |r| {
          Self::pending_op_success(info, rv_map, r)
        });
        return None;
      }

      // We poll every future here because it's much faster to return a result than
      // spin the event loop to get it.
      let mut pinned = self.allocate(op);
      match pinned.poll_unpin(&mut Context::from_waker(noop_waker_ref())) {
        Poll::Pending => self.spawn_polled(pinned, move |res| {
          Self::pending_op_success(info, rv_map, res)
        }),
        Poll::Ready(res) => {
          if DEFERRED {
            self.spawn_ready(Self::pending_op_success(info, rv_map, res))
          } else {
            return Some(res);
          }
        }
      };

      None
    }
  }

  #[inline(always)]
  fn poll_ready<'s>(
    &self,
    cx: &mut Context,
    scope: &mut v8::HandleScope<'s>,
  ) -> Poll<(
    PromiseId,
    OpId,
    bool,
    Result<v8::Local<'s, v8::Value>, v8::Local<'s, v8::Value>>,
  )> {
    let mut pending_ops = self.pending_ops.borrow_mut();
    if pending_ops.is_empty() {
      unsafe {
        if let Some(waker) =
          self.waker.get().as_ref().unwrap_unchecked().as_ref()
        {
          if !waker.will_wake(cx.waker()) {
            *self.waker.get() = Some(cx.waker().clone());
          }
        } else {
          *self.waker.get() = Some(cx.waker().clone());
        }
        return Poll::Pending;
      }
    }
    let item = ready!(pending_ops.poll_next_unpin(cx));
    let PendingOp(PendingOpInfo(promise_id, op_id, metrics_event), resp) =
      match item {
        Some(x) => x,
        None => {
          // If this task is really errored, things could be pretty bad
          panic!("Unrecoverable error: op panicked");
        }
      };

    let was_error = matches!(resp, OpResult::Err(_));
    let res = match resp.into_v8(scope) {
      Ok(v) => {
        if was_error {
          Err(v)
        } else {
          Ok(v)
        }
      }
      Err(e) => Err(
        OpResult::Err(OpError::new(&|_| "TypeError", e.into()))
          .into_v8(scope)
          .unwrap(),
      ),
    };
    Poll::Ready((promise_id, op_id, metrics_event, res))
  }

  #[inline(always)]
  fn len(&self) -> usize {
    self.pending_ops.borrow().len()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_double_free() {
    let driver = FuturesUnorderedDriver::default();
    let f = driver.allocate(async { 1 });
    drop(f);
    let f = driver.allocate(Box::pin(async { 1 }));
    drop(f);
    let f = driver.allocate(ready(Box::new(1)));
    drop(f);
  }

  #[test]
  fn test_exceed_arena() {
    let driver = FuturesUnorderedDriver::default();
    let mut v = vec![];
    for _ in 0..1000 {
      v.push(driver.allocate(ready(Box::new(1))));
    }
    drop(v);
  }

  #[test]
  fn test_drop_after_joinset() {
    let driver = FuturesUnorderedDriver::default();
    let mut v = vec![];
    for _ in 0..1000 {
      v.push(driver.allocate(ready(Box::new(1))));
    }
    drop(driver);
    drop(v);
  }
}
