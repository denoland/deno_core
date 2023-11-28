// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.

use crate::ops::OpCtx;
use crate::serde::Serialize;
use crate::OpDecl;
use crate::OpId;
use std::cell::Ref;
use std::cell::RefCell;
use std::cell::RefMut;
use std::rc::Rc;

/// The type of op metrics event.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum OpMetricsEvent {
  /// Entered an op dispatch.
  Dispatched,
  /// Left an op synchronously.
  Completed,
  /// Left an op asynchronously.
  CompletedAsync,
  /// Left an op synchronously with an exception.
  Error,
  /// Left an op asynchronously with an exception.
  ErrorAsync,
}

/// A callback to receieve an [`OpMetricsEvent`].
pub type OpMetricsFn = Rc<dyn Fn(&OpCtx, OpMetricsEvent)>;

// TODO(mmastrac): this would be better as a trait
/// A callback to retrieve an optional [`OpMetricsFn`] for this op.
pub type OpMetricsFactoryFn =
  Box<dyn Fn(OpId, usize, &OpDecl) -> Option<OpMetricsFn>>;

#[doc(hidden)]
pub fn dispatch_metrics_fast(opctx: &OpCtx, metrics: OpMetricsEvent) {
  // SAFETY: this should only be called from ops where we know the function is Some
  unsafe { (opctx.metrics_fn.as_ref().unwrap_unchecked())(opctx, metrics) }
}

#[doc(hidden)]
pub fn dispatch_metrics_slow(opctx: &OpCtx, metrics: OpMetricsEvent) {
  // SAFETY: this should only be called from ops where we know the function is Some
  unsafe { (opctx.metrics_fn.as_ref().unwrap_unchecked())(opctx, metrics) }
}

#[doc(hidden)]
pub fn dispatch_metrics_async(opctx: &OpCtx, metrics: OpMetricsEvent) {
  // SAFETY: this should only be called from ops where we know the function is Some
  unsafe { (opctx.metrics_fn.as_ref().unwrap_unchecked())(opctx, metrics) }
}

/// Used for both aggregate and per-op metrics.
#[derive(Clone, Default, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OpMetricsSummary {
  pub ops_dispatched_sync: u64,
  pub ops_dispatched_async: u64,
  pub ops_completed_async: u64,
}

impl OpMetricsSummary {
  /// Does this op have outstanding async op dispatches?
  pub fn has_outstanding_ops(&self) -> bool {
    self.ops_dispatched_async > self.ops_completed_async
  }
}

#[derive(Default, Debug)]
pub struct OpMetricsSummaryTracker {
  ops: RefCell<Vec<OpMetricsSummary>>,
}

impl OpMetricsSummaryTracker {
  pub fn per_op(&self) -> Ref<'_, Vec<OpMetricsSummary>> {
    self.ops.borrow()
  }

  pub fn aggregate(&self) -> OpMetricsSummary {
    let mut sum = OpMetricsSummary::default();

    for metrics in self.ops.borrow().iter() {
      sum.ops_dispatched_sync += metrics.ops_dispatched_sync;
      sum.ops_dispatched_async += metrics.ops_dispatched_async;
      sum.ops_completed_async += metrics.ops_completed_async;
    }

    sum
  }

  #[inline]
  fn metrics_mut(&self, id: OpId) -> RefMut<OpMetricsSummary> {
    RefMut::map(self.ops.borrow_mut(), |ops| &mut ops[id as usize])
  }

  /// Returns a [`OpMetricsFn`] for this tracker.
  fn op_metrics_fn(self: Rc<Self>) -> OpMetricsFn {
    Rc::new(move |ctx, event| match event {
      OpMetricsEvent::Dispatched => {
        if ctx.decl.is_async {
          self.metrics_mut(ctx.id).ops_dispatched_async += 1;
        } else {
          self.metrics_mut(ctx.id).ops_dispatched_sync += 1;
        }
      }
      OpMetricsEvent::Completed
      | OpMetricsEvent::Error
      | OpMetricsEvent::CompletedAsync
      | OpMetricsEvent::ErrorAsync => {
        if ctx.decl.is_async {
          self.metrics_mut(ctx.id).ops_completed_async += 1;
        }
      }
    })
  }

  /// Retrieves the metrics factory function for this tracker.
  pub fn op_metrics_factory_fn(
    self: Rc<Self>,
    op_enabled: impl Fn(&OpDecl) -> bool + 'static,
  ) -> OpMetricsFactoryFn {
    Box::new(move |_, total, op| {
      let mut ops = self.ops.borrow_mut();
      if ops.capacity() == 0 {
        ops.reserve_exact(total);
      }
      ops.push(OpMetricsSummary::default());
      if op_enabled(op) {
        Some(self.clone().op_metrics_fn())
      } else {
        None
      }
    })
  }
}
