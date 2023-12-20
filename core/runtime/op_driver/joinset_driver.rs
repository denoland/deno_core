// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use super::erased_future::ErasedFuture;
use super::OpDriver;
use crate::OpError;
use crate::OpMetricsEvent;
use crate::OpResult;
use crate::_ops::dispatch_metrics_async;
use crate::arena::ArenaBox;
use crate::arena::ArenaUnique;
use crate::ops::OpCtx;
use crate::ops::PendingOp;
use crate::runtime::ContextState;
use anyhow::Error;
use deno_unsync::JoinSet;
use futures::task::noop_waker_ref;
use futures::FutureExt;
use std::cell::RefCell;
use std::future::ready;
use std::future::Future;
use std::pin::Pin;
use std::task::Context;
use std::task::Poll;

const MAX_ARENA_FUTURE_SIZE: usize = 1024;

#[derive(Default)]
pub struct JoinSetDriver {
  pending_ops: RefCell<JoinSet<PendingOp>>,
  arena: ArenaUnique<ErasedFuture<MAX_ARENA_FUTURE_SIZE, ()>, 256>,
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
    unsafe {
      match self.get_unchecked_mut() {
        Self::Arena(ptr) => {
          let pin = Pin::new_unchecked(&mut **ptr);
          pin.poll(cx)
        }
        Self::Box(f) => f.poll_unpin(cx),
      }
    }
  }
}

impl JoinSetDriver {
  /// Allocate a future to run in this `JoinSet`.
  ///
  /// # Safety
  ///
  /// All allocations _must_ be placed in the JoinSet.
  fn allocate<F, R>(&self, f: F) -> FutureAllocation<R>
  where
    F: Future<Output = R> + 'static,
  {
    if std::mem::size_of::<F>() > MAX_ARENA_FUTURE_SIZE {
      FutureAllocation::Box(f.boxed_local())
    } else {
      unsafe {
        FutureAllocation::Arena(std::mem::transmute(self.arena.allocate(
          std::mem::transmute(ErasedFuture::<MAX_ARENA_FUTURE_SIZE, _>::new(f)),
        )))
      }
    }
  }

  #[inline(always)]
  fn spawn(&self, task: impl Future<Output = PendingOp> + 'static) {
    self.pending_ops.borrow_mut().spawn(task);
  }
}

impl OpDriver for JoinSetDriver {
  #[inline(always)]
  fn submit_op_fallible<R: 'static, E: Into<Error> + 'static>(
    &self,
    ctx: &OpCtx,
    lazy: bool,
    deferred: bool,
    promise_id: i32,
    op: impl Future<Output = Result<R, E>> + 'static,
    rv_map: for<'r> fn(
      &mut v8::HandleScope<'r>,
      R,
    )
      -> Result<v8::Local<'r, v8::Value>, serde_v8::Error>,
  ) -> Option<Result<R, E>> {
    {
      let id = ctx.id;
      let metrics = ctx.metrics_enabled();

      if lazy {
        let get_class = ctx.get_error_class_fn;
        self.spawn(op.map(move |r| match r {
          Ok(v) => PendingOp(
            promise_id,
            id,
            OpResult::Op2Temp(Box::new(move |scope| rv_map(scope, v))),
            metrics,
          ),
          Err(err) => PendingOp(
            promise_id,
            id,
            OpResult::Err(OpError::new(get_class, err.into())),
            metrics,
          ),
        }));
        return None;
      }

      // TODO(mmastrac): We have to poll every future here because that assumption is baked into a large number
      // of ops.
      let mut pinned = self.allocate(op.map(move |res| (promise_id, id, res)));

      let pinned =
        match pinned.poll_unpin(&mut Context::from_waker(noop_waker_ref())) {
          Poll::Pending => pinned,
          Poll::Ready(res) => {
            // TODO(mmastrac): optimize this so we don't double-allocate
            if deferred {
              self.allocate(ready(res))
            } else {
              return Some(res.2);
            }
          }
        };

      let get_class = ctx.get_error_class_fn;
      self.spawn(pinned.map(move |r| match r.2 {
        Ok(v) => PendingOp(
          r.0,
          r.1,
          OpResult::Op2Temp(Box::new(move |scope| rv_map(scope, v))),
          metrics,
        ),
        Err(err) => PendingOp(
          r.0,
          r.1,
          OpResult::Err(OpError::new(get_class, err.into())),
          metrics,
        ),
      }));
      None
    }
  }

  #[inline(always)]
  fn submit_op_infallible<R: 'static>(
    &self,
    ctx: &OpCtx,
    lazy: bool,
    deferred: bool,
    promise_id: i32,
    op: impl Future<Output = R> + 'static,
    rv_map: for<'r> fn(
      &mut v8::HandleScope<'r>,
      R,
    )
      -> Result<v8::Local<'r, v8::Value>, serde_v8::Error>,
  ) -> Option<R> {
    {
      let id = ctx.id;
      let metrics = ctx.metrics_enabled();
      if lazy {
        let mapper = move |r| {
          PendingOp(
            promise_id,
            id,
            OpResult::Op2Temp(Box::new(move |scope| rv_map(scope, r))),
            metrics,
          )
        };
        self.spawn(op.map(mapper));
        return None;
      }

      // TODO(mmastrac): we poll every future here because it's much faster to return a result than
      // spin the event loop to get it.
      let mut pinned = self.allocate(op);
      match pinned.poll_unpin(&mut Context::from_waker(noop_waker_ref())) {
        Poll::Pending => self.spawn(pinned.map(move |res| {
          PendingOp(
            promise_id,
            id,
            OpResult::Op2Temp(Box::new(move |scope| rv_map(scope, res))),
            metrics,
          )
        })),
        Poll::Ready(res) => {
          if deferred {
            self.spawn(ready(PendingOp(
              promise_id,
              id,
              OpResult::Op2Temp(Box::new(move |scope| rv_map(scope, res))),
              metrics,
            )))
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
    context_state: &ContextState,
    args: &mut smallvec::SmallVec<[v8::Local<'s, v8::Value>; 32]>,
  ) -> bool {
    let mut dispatched_ops = false;
    loop {
      let Poll::Ready(item) = self.pending_ops.borrow_mut().poll_join_next(cx)
      else {
        break;
      };
      // TODO(mmastrac): If this task is really errored, things could be pretty bad
      let PendingOp(promise_id, op_id, resp, metrics_event) = item.unwrap();
      context_state.unrefed_ops.borrow_mut().remove(&promise_id);
      dispatched_ops |= true;
      args.push(v8::Integer::new(scope, promise_id).into());
      let was_error = matches!(resp, OpResult::Err(_));
      let res = resp.to_v8(scope);
      if metrics_event {
        if res.is_ok() && !was_error {
          dispatch_metrics_async(
            &context_state.op_ctxs.borrow()[op_id as usize],
            OpMetricsEvent::CompletedAsync,
          );
        } else {
          dispatch_metrics_async(
            &context_state.op_ctxs.borrow()[op_id as usize],
            OpMetricsEvent::ErrorAsync,
          );
        }
      }
      args.push(match res {
        Ok(v) => v,
        Err(e) => OpResult::Err(OpError::new(&|_| "TypeError", e.into()))
          .to_v8(scope)
          .unwrap(),
      });
    }
    dispatched_ops
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
}
