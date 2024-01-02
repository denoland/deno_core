// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use super::future_arena::FutureAllocation;
use super::future_arena::FutureArena;
use super::op_results::*;
use super::OpDriver;
use super::RetValMapper;
use crate::ops::OpCtx;
use crate::GetErrorClassFn;
use crate::OpId;
use crate::PromiseId;
use anyhow::Error;
use deno_unsync::JoinSet;
use futures::task::noop_waker_ref;
use futures::FutureExt;
use std::cell::RefCell;
use std::future::ready;
use std::future::Future;
use std::task::ready;
use std::task::Context;
use std::task::Poll;

/// [`OpDriver`] implementation built on a tokio [`JoinSet`].
#[derive(Default)]
pub struct JoinSetDriver {
  pending_ops: RefCell<JoinSet<PendingOp>>,
  arena: FutureArena,
}

impl JoinSetDriver {
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
      let mut pinned = self.arena.allocate(op);
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
      let mut pinned = self.arena.allocate(op);
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
        Err(_) => {
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
