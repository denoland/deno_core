// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use super::ContextState;
use crate::ops::OpCtx;
use anyhow::Error;
use smallvec::SmallVec;
use std::future::Future;
use std::task::Context;

pub mod erased_future;
pub mod joinset_driver;

pub type RetValMapper<R> =
  for<'r> fn(
    &mut v8::HandleScope<'r>,
    R,
  ) -> Result<v8::Local<'r, v8::Value>, serde_v8::Error>;

/// `OpDriver` encapsulates the interface for handling operations within Deno's runtime.
///
/// This trait defines methods for submitting ops and polling readiness inside of the
/// event loop.
pub(crate) trait OpDriver: Default {
  /// Submits an operation that is expected to complete successfully without errors.
  fn submit_op_infallible<R: 'static, const LAZY: bool, const DEFERRED: bool>(
    &self,
    ctx: &OpCtx,
    promise_id: i32,
    op: impl Future<Output = R> + 'static,
    rv_map: RetValMapper<R>,
  ) -> Option<R>;

  /// Submits an operation that may produce errors during execution.
  ///
  /// This method is similar to `submit_op_infallible` but is used when the op
  /// might return an error (`Result`).
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
  ) -> Option<Result<R, E>>;

  /// Polls the readiness of the operation driver for handling a new operation.
  // TODO(mmastrac): Remove ContextState if possible
  fn poll_ready<'s>(
    &self,
    cx: &mut Context,
    scope: &mut v8::HandleScope<'s>,
    context_state: &ContextState,
    args: &mut SmallVec<[v8::Local<'s, v8::Value>; 32]>,
  ) -> bool;

  fn len(&self) -> usize;
}
