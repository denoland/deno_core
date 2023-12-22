// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use super::erased_future::ErasedFuture;
use super::erased_future::TypeErased;
use super::OpDriver;
use super::RetValMapper;
use crate::arena::ArenaBox;
use crate::arena::ArenaUnique;
use crate::ops::OpCtx;
use crate::GetErrorClassFn;
use crate::OpId;
use crate::PromiseId;
use anyhow::Error;
use deno_unsync::JoinSet;
use futures::task::noop_waker_ref;
use futures::FutureExt;
use serde::Serialize;
use std::cell::RefCell;
use std::future::ready;
use std::future::Future;
use std::pin::Pin;
use std::task::ready;
use std::task::Context;
use std::task::Poll;

const MAX_ARENA_FUTURE_SIZE: usize = 1024;
const FUTURE_ARENA_COUNT: usize = 256;
const MAX_RESULT_SIZE: usize = 32;

struct PendingOp(pub PendingOpInfo, pub OpResult);

struct PendingOpInfo(pub PromiseId, pub OpId, pub bool);

#[allow(clippy::type_complexity)]
struct OpValue {
  value: TypeErased<MAX_RESULT_SIZE>,
  rv_map: *const fn(),
  map_fn: for<'a> fn(
    scope: &mut v8::HandleScope<'a>,
    rv_map: *const fn(),
    value: TypeErased<MAX_RESULT_SIZE>,
  ) -> Result<v8::Local<'a, v8::Value>, serde_v8::Error>,
}

impl OpValue {
  fn new<R: 'static>(rv_map: RetValMapper<R>, v: R) -> Self {
    Self {
      value: TypeErased::new(v),
      rv_map: rv_map as _,
      map_fn: |scope, rv_map, erased| unsafe {
        let r = erased.take();
        let rv_map: RetValMapper<R> = std::mem::transmute(rv_map);
        rv_map(scope, r)
      },
    }
  }
}

type ValueLargeFn =
  dyn for<'a> FnOnce(
    &mut v8::HandleScope<'a>,
  ) -> Result<v8::Local<'a, v8::Value>, serde_v8::Error>;

enum OpResult {
  /// Errors.
  Err(OpError),
  /// For small ops, we include them in an erased type container.
  Value(OpValue),
  /// For ops that return "large" results (> MAX_RESULT_SIZE bytes) we just box a function
  /// that can turn it into a v8 value.
  ValueLarge(Box<ValueLargeFn>),
}

impl OpResult {
  pub fn into_v8<'a>(
    self,
    scope: &mut v8::HandleScope<'a>,
  ) -> Result<v8::Local<'a, v8::Value>, serde_v8::Error> {
    match self {
      Self::Err(err) => serde_v8::to_v8(scope, err),
      Self::Value(f) => (f.map_fn)(scope, f.rv_map, f.value),
      Self::ValueLarge(f) => f(scope),
    }
  }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpError {
  #[serde(rename = "$err_class_name")]
  class_name: &'static str,
  message: String,
  code: Option<&'static str>,
}

impl OpError {
  pub fn new(get_class: GetErrorClassFn, err: Error) -> Self {
    Self {
      class_name: (get_class)(&err),
      message: format!("{err:#}"),
      code: crate::error_codes::get_error_code(&err),
    }
  }
}

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

/// [`OpDriver`] implementation built on a tokio [`JoinSet`].
pub struct JoinSetDriver {
  pending_ops: RefCell<JoinSet<PendingOp>>,
  arena: ArenaUnique<ErasedFuture<MAX_ARENA_FUTURE_SIZE, ()>>,
}

impl Default for JoinSetDriver {
  fn default() -> Self {
    Self {
      pending_ops: Default::default(),
      arena: ArenaUnique::with_capacity(FUTURE_ARENA_COUNT),
    }
  }
}

impl JoinSetDriver {
  /// Allocate a future to run in this `JoinSet`. If the future is too large, or the arena
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

  /// Spawn an unpolled task, along with a function that can map it to a [`PendingOp`].
  #[inline(always)]
  fn spawn_unpolled<R>(
    &self,
    task: impl Future<Output = R> + 'static,
    map: impl FnOnce(R) -> PendingOp + 'static,
  ) {
    self.pending_ops.borrow_mut().spawn(task.map(map));
  }

  /// Spawn a ready task that already has a [`PendingOp`].
  #[inline(always)]
  fn spawn_ready(&self, ready_op: PendingOp) {
    self.pending_ops.borrow_mut().spawn(ready(ready_op));
  }

  /// Spawn a polled task inside a [`FutureAllocation`], along with a function that can map it to a [`PendingOp`].
  #[inline(always)]
  fn spawn_polled<R>(
    &self,
    task: FutureAllocation<R>,
    map: impl FnOnce(R) -> PendingOp + 'static,
  ) {
    self.pending_ops.borrow_mut().spawn(task.map(map));
  }

  #[inline(always)]
  fn pending_op_success<R: 'static>(
    info: PendingOpInfo,
    rv_map: RetValMapper<R>,
    v: R,
  ) -> PendingOp {
    if std::mem::size_of::<R>() > MAX_RESULT_SIZE {
      PendingOp(
        info,
        OpResult::ValueLarge(Box::new(move |scope| rv_map(scope, v))),
      )
    } else {
      PendingOp(info, OpResult::Value(OpValue::new(rv_map, v)))
    }
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

impl OpDriver for JoinSetDriver {
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
    let item = ready!(self.pending_ops.borrow_mut().poll_join_next(cx));
    let PendingOp(PendingOpInfo(promise_id, op_id, metrics_event), resp) =
      match item {
        Ok(x) => x,
        Err(e) => {
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
    let driver = JoinSetDriver::default();
    let f = driver.allocate(async { 1 });
    drop(f);
    let f = driver.allocate(Box::pin(async { 1 }));
    drop(f);
    let f = driver.allocate(ready(Box::new(1)));
    drop(f);
  }

  #[test]
  fn test_exceed_arena() {
    let driver = JoinSetDriver::default();
    let mut v = vec![];
    for _ in 0..1000 {
      v.push(driver.allocate(ready(Box::new(1))));
    }
    drop(v);
  }

  #[test]
  fn test_drop_after_joinset() {
    let driver = JoinSetDriver::default();
    let mut v = vec![];
    for _ in 0..1000 {
      v.push(driver.allocate(ready(Box::new(1))));
    }
    drop(driver);
    drop(v);
  }
}
