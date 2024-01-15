// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use super::future_arena::FutureAllocation;
use super::future_arena::FutureArena;
use super::op_results::*;
use super::OpDriver;
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
pub struct JoinSetDriver<C: OpMappingContext = V8OpMappingContext> {
  pending_ops: RefCell<JoinSet<PendingOp<C>>>,
  arena: FutureArena,
}

impl<C: OpMappingContext> Default for JoinSetDriver<C> {
  fn default() -> Self {
    Self {
      pending_ops: Default::default(),
      arena: Default::default(),
    }
  }
}

impl<C: OpMappingContext> JoinSetDriver<C> {
  /// Spawn an unpolled task, along with a function that can map it to a [`PendingOp`].
  #[inline(always)]
  fn spawn_unpolled<R>(
    &self,
    task: impl Future<Output = R> + 'static,
    map: impl FnOnce(R) -> PendingOp<C> + 'static,
  ) {
    self.pending_ops.borrow_mut().spawn(task.map(map));
  }

  /// Spawn a ready task that already has a [`PendingOp`].
  #[inline(always)]
  fn spawn_ready(&self, ready_op: PendingOp<C>) {
    self.pending_ops.borrow_mut().spawn(ready(ready_op));
  }

  /// Spawn a polled task inside a [`FutureAllocation`], along with a function that can map it to a [`PendingOp`].
  #[inline(always)]
  fn spawn_polled<R>(
    &self,
    task: FutureAllocation<R>,
    map: impl FnOnce(R) -> PendingOp<C> + 'static,
  ) {
    self.pending_ops.borrow_mut().spawn(task.map(map));
  }
}

impl<C: OpMappingContext> OpDriver<C> for JoinSetDriver<C> {
  fn submit_op_fallible<
    R: 'static,
    E: Into<Error> + 'static,
    const LAZY: bool,
    const DEFERRED: bool,
  >(
    &self,
    op_id: OpId,
    promise_id: i32,
    op: impl Future<Output = Result<R, E>> + 'static,
    rv_map: C::MappingFn<R>,
  ) -> Option<Result<R, E>> {
    {
      let info = PendingOpInfo(promise_id, op_id);

      if LAZY {
        self.spawn_unpolled(op, move |r| PendingOp::new(info, rv_map, r));
        return None;
      }

      // We poll every future here because it's much faster to return a result than
      // spin the event loop to get it.
      let mut pinned = self.arena.allocate(op);
      match pinned.poll_unpin(&mut Context::from_waker(noop_waker_ref())) {
        Poll::Pending => {
          self.spawn_polled(pinned, move |r| PendingOp::new(info, rv_map, r))
        }
        Poll::Ready(res) => {
          if DEFERRED {
            self.spawn_ready(PendingOp::new(info, rv_map, res))
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
    op_id: OpId,
    promise_id: i32,
    op: impl Future<Output = R> + 'static,
    rv_map: C::MappingFn<R>,
  ) -> Option<R> {
    {
      let info = PendingOpInfo(promise_id, op_id);
      if LAZY {
        self.spawn_unpolled(op, move |r| PendingOp::ok(info, rv_map, r));
        return None;
      }

      // We poll every future here because it's much faster to return a result than
      // spin the event loop to get it.
      let mut pinned = self.arena.allocate(op);
      match pinned.poll_unpin(&mut Context::from_waker(noop_waker_ref())) {
        Poll::Pending => {
          self.spawn_polled(pinned, move |res| PendingOp::ok(info, rv_map, res))
        }
        Poll::Ready(res) => {
          if DEFERRED {
            self.spawn_ready(PendingOp::ok(info, rv_map, res))
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
  ) -> Poll<(PromiseId, OpId, OpResult<C>)> {
    let item = ready!(self.pending_ops.borrow_mut().poll_join_next(cx));
    let PendingOp(PendingOpInfo(promise_id, op_id), resp) = match item {
      Ok(x) => x,
      Err(_) => {
        // If this task is really errored, things could be pretty bad
        panic!("Unrecoverable error: op panicked");
      }
    };
    Poll::Ready((promise_id, op_id, resp))
  }

  #[inline(always)]
  fn len(&self) -> usize {
    self.pending_ops.borrow().len()
  }
}
