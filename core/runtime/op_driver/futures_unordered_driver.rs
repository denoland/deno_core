// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use super::future_arena::FutureAllocation;
use super::future_arena::FutureArena;
use super::op_results::*;
use super::OpDriver;
use crate::GetErrorClassFn;
use crate::OpId;
use crate::PromiseId;
use anyhow::Error;
use deno_unsync::spawn;
use deno_unsync::JoinHandle;
use deno_unsync::UnsyncWaker;
use futures::future::poll_fn;
use futures::stream::FuturesUnordered;
use futures::task::noop_waker_ref;
use futures::FutureExt;
use futures::Stream;
use std::cell::Cell;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::future::ready;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::ready;
use std::task::Context;
use std::task::Poll;

async fn poll_task<C: OpMappingContext>(
  mut results: SubmissionQueueResults<
    FuturesUnordered<FutureAllocation<PendingOp<C>>>,
  >,
  tx: Rc<RefCell<VecDeque<PendingOp<C>>>>,
  tx_waker: Rc<UnsyncWaker>,
) {
  loop {
    let ready = poll_fn(|cx| results.poll_next_unpin(cx)).await;
    tx.borrow_mut().push_back(ready);
    tx_waker.wake_by_ref();
  }
}

#[derive(Default)]
enum MaybeTask {
  #[default]
  Empty,
  Task(Pin<Box<dyn Future<Output = ()>>>),
  Handle(JoinHandle<()>),
}

/// [`OpDriver`] implementation built on a tokio [`JoinSet`].
pub struct FuturesUnorderedDriver<
  C: OpMappingContext + 'static = V8OpMappingContext,
> {
  task: Cell<MaybeTask>,
  task_set: Cell<bool>,
  queue: SubmissionQueue<FuturesUnordered<FutureAllocation<PendingOp<C>>>>,
  completed_ops: Rc<RefCell<VecDeque<PendingOp<C>>>>,
  completed_waker: Rc<UnsyncWaker>,
  arena: FutureArena,
}

impl<C: OpMappingContext + 'static> Drop for FuturesUnorderedDriver<C> {
  fn drop(&mut self) {
    if let MaybeTask::Handle(h) = self.task.take() {
      h.abort()
    }
  }
}

impl<C: OpMappingContext> Default for FuturesUnorderedDriver<C> {
  fn default() -> Self {
    let (queue, results) = new_submission_queue();
    let completed_ops = Rc::new(RefCell::new(VecDeque::with_capacity(128)));
    let completed_waker = Rc::new(UnsyncWaker::default());
    let task = MaybeTask::Task(Box::pin(poll_task(
      results,
      completed_ops.clone(),
      completed_waker.clone(),
    )))
    .into();

    Self {
      task,
      task_set: Default::default(),
      completed_ops,
      queue,
      completed_waker,
      arena: Default::default(),
    }
  }
}

impl<C: OpMappingContext> FuturesUnorderedDriver<C> {
  #[inline(always)]
  fn ensure_task(&self) {
    if !self.task_set.get() {
      self.spawn_task();
    }
  }

  #[inline(never)]
  #[cold]
  fn spawn_task(&self) {
    let MaybeTask::Task(task) = self.task.replace(Default::default()) else {
      unreachable!()
    };
    self.task.set(MaybeTask::Handle(spawn(task)));
    self.task_set.set(true);
  }

  /// Spawn an unpolled task, along with a function that can map it to a [`PendingOp`].
  #[inline(always)]
  fn spawn_unpolled<R>(
    &self,
    task: impl Future<Output = R> + 'static,
    map: impl FnOnce(R) -> PendingOp<C> + 'static,
  ) {
    self.ensure_task();
    self.queue.spawn(self.arena.allocate(task.map(map)));
  }

  /// Spawn a ready task that already has a [`PendingOp`].
  #[inline(always)]
  fn spawn_ready(&self, ready_op: PendingOp<C>) {
    self.ensure_task();
    self.queue.spawn(self.arena.allocate(ready(ready_op)));
  }

  /// Spawn a polled task inside a [`FutureAllocation`], along with a function that can map it to a [`PendingOp`].
  #[inline(always)]
  fn spawn_polled<R>(
    &self,
    task: FutureAllocation<R>,
    map: impl FnOnce(R) -> PendingOp<C> + 'static,
  ) {
    self.ensure_task();
    self.queue.spawn(self.arena.allocate(task.map(map)));
  }

  #[inline(always)]
  fn pending_op_success<R: 'static>(
    info: PendingOpInfo,
    rv_map: C::MappingFn<R>,
    v: R,
  ) -> PendingOp<C> {
    PendingOp(info, OpResult::new_value(v, rv_map))
  }

  #[inline(always)]
  fn pending_op_failure<E: Into<Error> + 'static>(
    info: PendingOpInfo,
    get_class: GetErrorClassFn,
    err: E,
  ) -> PendingOp<C> {
    PendingOp(info, OpResult::Err(OpError::new(get_class, err.into())))
  }

  #[inline(always)]
  fn pending_op_result<R: 'static, E: Into<Error> + 'static>(
    info: PendingOpInfo,
    rv_map: C::MappingFn<R>,
    get_class: GetErrorClassFn,
    result: Result<R, E>,
  ) -> PendingOp<C> {
    match result {
      Ok(r) => Self::pending_op_success(info, rv_map, r),
      Err(err) => Self::pending_op_failure(info, get_class, err),
    }
  }
}

impl<C: OpMappingContext> OpDriver<C> for FuturesUnorderedDriver<C> {
  fn submit_op_fallible<
    R: 'static,
    E: Into<Error> + 'static,
    const LAZY: bool,
    const DEFERRED: bool,
  >(
    &self,
    op_id: OpId,
    metrics_enabled: bool,
    get_class: GetErrorClassFn,
    promise_id: i32,
    op: impl Future<Output = Result<R, E>> + 'static,
    rv_map: C::MappingFn<R>,
  ) -> Option<Result<R, E>> {
    {
      let info = PendingOpInfo(promise_id, op_id, metrics_enabled);

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
    op_id: OpId,
    metrics_enabled: bool,
    promise_id: i32,
    op: impl Future<Output = R> + 'static,
    rv_map: C::MappingFn<R>,
  ) -> Option<R> {
    {
      let info = PendingOpInfo(promise_id, op_id, metrics_enabled);
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
  fn poll_ready(
    &self,
    cx: &mut Context,
  ) -> Poll<(PromiseId, OpId, bool, OpResult<C>)> {
    let mut ops = self.completed_ops.borrow_mut();
    if ops.is_empty() {
      self.completed_waker.register(cx.waker());
      return Poll::Pending;
    }
    let item = ops.pop_front().unwrap();
    let PendingOp(PendingOpInfo(promise_id, op_id, metrics_event), resp) = item;
    Poll::Ready((promise_id, op_id, metrics_event, resp))
  }

  #[inline(always)]
  fn len(&self) -> usize {
    self.queue.len()
  }
}

impl<F: Future<Output = R>, R> SubmissionQueueFutures for FuturesUnordered<F> {
  type Future = F;
  type Output = F::Output;

  fn len(&self) -> usize {
    self.len()
  }

  fn spawn(&mut self, f: Self::Future) {
    self.push(f)
  }

  fn poll_next_unpin(&mut self, cx: &mut Context) -> Poll<Self::Output> {
    Poll::Ready(ready!(Pin::new(self).poll_next(cx)).unwrap())
  }
}

#[derive(Default)]
struct Queue<F: SubmissionQueueFutures> {
  queue: RefCell<F>,
  item_waker: UnsyncWaker,
}

pub trait SubmissionQueueFutures: Default {
  type Future: Future<Output = Self::Output>;
  type Output;

  fn len(&self) -> usize;
  fn spawn(&mut self, f: Self::Future);
  fn poll_next_unpin(&mut self, cx: &mut Context) -> Poll<Self::Output>;
}

pub struct SubmissionQueueResults<F: SubmissionQueueFutures> {
  queue: Rc<Queue<F>>,
}

impl<F: SubmissionQueueFutures> SubmissionQueueResults<F> {
  pub fn poll_next_unpin(&mut self, cx: &mut Context) -> Poll<F::Output> {
    let mut queue = self.queue.queue.borrow_mut();
    self.queue.item_waker.register(cx.waker());
    if queue.len() == 0 {
      return Poll::Pending;
    }
    queue.poll_next_unpin(cx)
  }
}

pub struct SubmissionQueue<F: SubmissionQueueFutures> {
  queue: Rc<Queue<F>>,
}

impl<F: SubmissionQueueFutures> SubmissionQueue<F> {
  pub fn spawn(&self, f: F::Future) {
    self.queue.queue.borrow_mut().spawn(f);
    self.queue.item_waker.wake_by_ref();
  }

  pub fn len(&self) -> usize {
    self.queue.queue.borrow().len()
  }
}

/// Create a [`SubmissionQueue`] and [`SubmissionQueueResults`] that allow for submission of tasks
/// and reception of task results. We may add work to the [`SubmissionQueue`] from any task, and the
/// [`SubmissionQueueResults`] will be polled from a single location.
pub fn new_submission_queue<F: SubmissionQueueFutures>(
) -> (SubmissionQueue<F>, SubmissionQueueResults<F>) {
  let queue: Rc<Queue<F>> = Default::default();
  (
    SubmissionQueue {
      queue: queue.clone(),
    },
    SubmissionQueueResults { queue },
  )
}
