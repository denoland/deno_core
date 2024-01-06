use std::cell::RefCell;
use std::pin::Pin;
use std::rc::Rc;
use std::task::ready;
use std::task::Context;
use std::task::Poll;

use deno_unsync::UnsyncWaker;
use futures::stream::FuturesUnordered;
use futures::Future;
use futures::Stream;

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
    self.queue.item_waker.register(&cx.waker());
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
