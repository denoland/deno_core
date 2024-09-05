// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

use crate::ops::OpCtx;
use crate::serde::Serialize;
use crate::OpDecl;
use crate::OpId;
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

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum OpMetricsSource {
  Slow,
  Fast,
  Async,
}

/// A callback to receieve an [`OpMetricsEvent`].
pub type OpMetricsFn = Rc<dyn Fn(&OpCtx, OpMetricsEvent, OpMetricsSource)>;

// TODO(mmastrac): this would be better as a trait
/// A callback to retrieve an optional [`OpMetricsFn`] for this op.
pub type OpMetricsFactoryFn =
  Box<dyn Fn(OpId, usize, &OpDecl) -> Option<OpMetricsFn>>;

/// Given two [`OpMetricsFactoryFn`] implementations, merges them so that op metric events are
/// called on both.
pub fn merge_op_metrics(
  fn1: impl Fn(OpId, usize, &OpDecl) -> Option<OpMetricsFn> + 'static,
  fn2: impl Fn(OpId, usize, &OpDecl) -> Option<OpMetricsFn> + 'static,
) -> OpMetricsFactoryFn {
  Box::new(move |op, count, decl| {
    match (fn1(op, count, decl), fn2(op, count, decl)) {
      (None, None) => None,
      (Some(a), None) => Some(a),
      (None, Some(b)) => Some(b),
      (Some(a), Some(b)) => Some(Rc::new(move |ctx, event, source| {
        a(ctx, event, source);
        b(ctx, event, source);
      })),
    }
  })
}

#[doc(hidden)]
pub fn dispatch_metrics_fast(opctx: &OpCtx, metrics: OpMetricsEvent) {
  // SAFETY: this should only be called from ops where we know the function is Some
  unsafe {
    (opctx.metrics_fn.as_ref().unwrap_unchecked())(
      opctx,
      metrics,
      OpMetricsSource::Fast,
    )
  }
}

#[doc(hidden)]
pub fn dispatch_metrics_slow(opctx: &OpCtx, metrics: OpMetricsEvent) {
  // SAFETY: this should only be called from ops where we know the function is Some
  unsafe {
    (opctx.metrics_fn.as_ref().unwrap_unchecked())(
      opctx,
      metrics,
      OpMetricsSource::Slow,
    )
  }
}

#[doc(hidden)]
pub fn dispatch_metrics_async(opctx: &OpCtx, metrics: OpMetricsEvent) {
  // SAFETY: this should only be called from ops where we know the function is Some
  unsafe {
    (opctx.metrics_fn.as_ref().unwrap_unchecked())(
      opctx,
      metrics,
      OpMetricsSource::Async,
    )
  }
}

/// Used for both aggregate and per-op metrics.
#[derive(Clone, Default, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OpMetricsSummary {
  // The number of ops dispatched synchronously
  pub ops_dispatched_sync: u64,
  // The number of ops dispatched asynchronously
  pub ops_dispatched_async: u64,
  // The number of sync ops dispatched fast
  pub ops_dispatched_fast: u64,
  // The number of asynchronously-dispatch ops completed
  pub ops_completed_async: u64,
}

impl OpMetricsSummary {
  /// Does this op have outstanding async op dispatches?
  pub fn has_outstanding_ops(&self) -> bool {
    self.ops_dispatched_async > self.ops_completed_async
  }
}
