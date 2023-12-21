// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use super::erased_future::ErasedFuture;
use super::OpDriver;
use crate::OpError;
use crate::OpId;
use crate::OpMetricsEvent;
use crate::OpResult;
use crate::PromiseId;
use crate::_ops::dispatch_metrics_async;
use crate::arena::RawArena;
use crate::ops::OpCtx;
use crate::runtime::ContextState;
use anyhow::Error;
use deno_unsync::JoinSet;
use futures::task::noop_waker_ref;
use futures::FutureExt;
use std::cell::RefCell;
use std::cell::UnsafeCell;
use std::future::ready;
use std::future::Future;
use std::pin::Pin;
use std::task::Context;
use std::task::Poll;

const MAX_ARENA_FUTURE_SIZE: usize = 1024;
const MAX_FUTURE_SIZE: usize = 256;

pub struct PendingOp(pub PendingOpInfo, pub OpResult);

pub struct PendingOpInfo(pub PromiseId, pub OpId, pub bool);

#[derive(Clone, Copy)]
#[repr(transparent)]
struct ArenaPtr(
  *mut Option<
    RawArena<ErasedFuture<MAX_ARENA_FUTURE_SIZE, ()>, MAX_FUTURE_SIZE>,
  >,
);

impl ArenaPtr {
  pub fn recycle<R>(self, ptr: *mut ErasedFuture<MAX_ARENA_FUTURE_SIZE, R>) {
    unsafe {
      if let Some(arena) = &*self.0 {
        arena.recycle(ptr as _);
      }
    }
  }

  fn allocate<F, R>(
    self,
    f: F,
  ) -> Result<*mut ErasedFuture<MAX_ARENA_FUTURE_SIZE, R>, F>
  where
    F: Future<Output = R> + 'static,
  {
    unsafe {
      let arena = (*self.0).as_ref();
      let alloc = arena.unwrap_unchecked().allocate_if_space();
      if alloc.is_null() {
        return Err(f);
      }
      std::ptr::write(
        alloc as _,
        ErasedFuture::<MAX_ARENA_FUTURE_SIZE, _>::new(f),
      );
      Ok(alloc as _)
    }
  }

  fn is_alive(self) -> bool {
    unsafe {
      let ptr = &*self.0;
      ptr.is_some()
    }
  }
}

enum FutureAllocation<R: 'static> {
  Arena(*mut ErasedFuture<MAX_ARENA_FUTURE_SIZE, R>, ArenaPtr),
  Box(Pin<Box<dyn Future<Output = R>>>),
}

impl<R> Drop for FutureAllocation<R> {
  fn drop(&mut self) {
    match self {
      Self::Arena(ptr, arena) => arena.recycle(*ptr),
      Self::Box(..) => {}
    }
  }
}

impl<R> Unpin for FutureAllocation<R> {}

impl<R> Future for FutureAllocation<R> {
  type Output = R;

  #[inline(always)]
  fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
    unsafe {
      match self.get_unchecked_mut() {
        Self::Arena(ptr, arena) => {
          if !arena.is_alive() {
            return Poll::Pending;
          }
          let pin = Pin::new_unchecked(&mut **ptr);
          pin.poll(cx)
        }
        Self::Box(f) => f.poll_unpin(cx),
      }
    }
  }
}

pub struct JoinSetDriver {
  pending_ops: RefCell<JoinSet<PendingOp>>,
  arena: UnsafeCell<
    Option<RawArena<ErasedFuture<MAX_ARENA_FUTURE_SIZE, ()>, MAX_FUTURE_SIZE>>,
  >,
}

impl Default for JoinSetDriver {
  fn default() -> Self {
    Self {
      pending_ops: Default::default(),
      arena: UnsafeCell::new(Some(Default::default())),
    }
  }
}

impl Drop for JoinSetDriver {
  fn drop(&mut self) {
    unsafe { *self.arena.get() = None }
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
      let arena = ArenaPtr(self.arena.get());
      match arena.allocate(f) {
        Ok(alloc) => FutureAllocation::Arena(alloc, arena),
        Err(f) => FutureAllocation::Box(f.boxed_local()),
      }
    }
  }

  #[inline(always)]
  fn spawn(&self, task: impl Future<Output = PendingOp> + 'static) {
    self.pending_ops.borrow_mut().spawn(task);
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
    rv_map: for<'r> fn(
      &mut v8::HandleScope<'r>,
      R,
    )
      -> Result<v8::Local<'r, v8::Value>, serde_v8::Error>,
  ) -> Option<Result<R, E>> {
    {
      let info = PendingOpInfo(promise_id, ctx.id, ctx.metrics_enabled());
      let get_class = ctx.get_error_class_fn;

      if LAZY {
        self.spawn(op.map(move |r| match r {
          Ok(v) => PendingOp(
            info,
            OpResult::Op2Temp(Box::new(move |scope| rv_map(scope, v))),
          ),
          Err(err) => {
            PendingOp(info, OpResult::Err(OpError::new(get_class, err.into())))
          }
        }));
        return None;
      }

      // TODO(mmastrac): we poll every future here because it's much faster to return a result than
      // spin the event loop to get it.
      let mut pinned = self.allocate(op);
      match pinned.poll_unpin(&mut Context::from_waker(noop_waker_ref())) {
        Poll::Pending => self.spawn(pinned.map(move |r| match r {
          Ok(v) => PendingOp(
            info,
            OpResult::Op2Temp(Box::new(move |scope| rv_map(scope, v))),
          ),
          Err(err) => {
            PendingOp(info, OpResult::Err(OpError::new(get_class, err.into())))
          }
        })),
        Poll::Ready(res) => {
          if DEFERRED {
            match res {
              Ok(v) => self.spawn(ready(PendingOp(
                info,
                OpResult::Op2Temp(Box::new(move |scope| rv_map(scope, v))),
              ))),
              Err(err) => self.spawn(ready(PendingOp(
                info,
                OpResult::Err(OpError::new(get_class, err.into())),
              ))),
            }
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
    rv_map: for<'r> fn(
      &mut v8::HandleScope<'r>,
      R,
    )
      -> Result<v8::Local<'r, v8::Value>, serde_v8::Error>,
  ) -> Option<R> {
    {
      let info = PendingOpInfo(promise_id, ctx.id, ctx.metrics_enabled());
      if LAZY {
        self.spawn(op.map(move |r| {
          PendingOp(
            info,
            OpResult::Op2Temp(Box::new(move |scope| rv_map(scope, r))),
          )
        }));
        return None;
      }

      // TODO(mmastrac): we poll every future here because it's much faster to return a result than
      // spin the event loop to get it.
      let mut pinned = self.allocate(op);
      match pinned.poll_unpin(&mut Context::from_waker(noop_waker_ref())) {
        Poll::Pending => self.spawn(pinned.map(move |res| {
          PendingOp(
            info,
            OpResult::Op2Temp(Box::new(move |scope| rv_map(scope, res))),
          )
        })),
        Poll::Ready(res) => {
          if DEFERRED {
            self.spawn(ready(PendingOp(
              info,
              OpResult::Op2Temp(Box::new(move |scope| rv_map(scope, res))),
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
      let PendingOp(PendingOpInfo(promise_id, op_id, metrics_event), resp) =
        item.unwrap();
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
