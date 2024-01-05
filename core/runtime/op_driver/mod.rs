// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use crate::ops::OpCtx;
use crate::OpId;
use crate::PromiseId;
use anyhow::Error;
use std::future::Future;
use std::task::Context;
use std::task::Poll;

mod erased_future;
mod future_arena;
mod futures_unordered_driver;
mod joinset_driver;
mod op_results;
mod submission_queue;

pub use futures_unordered_driver::FuturesUnorderedDriver;
pub use joinset_driver::JoinSetDriver;

pub use self::op_results::OpMappingContext;
use self::op_results::OpResult;
pub use self::op_results::V8OpMappingContext;
pub use self::op_results::V8RetValMapper;

/// `OpDriver` encapsulates the interface for handling operations within Deno's runtime.
///
/// This trait defines methods for submitting ops and polling readiness inside of the
/// event loop.
///
/// The driver takes an optional [`OpMappingContext`] implementation, which defaults to
/// one compatible with v8. This is used solely for testing purposes.
pub(crate) trait OpDriver<C: OpMappingContext = V8OpMappingContext>:
  Default
{
  /// Submits an operation that is expected to complete successfully without errors.
  fn submit_op_infallible<R: 'static, const LAZY: bool, const DEFERRED: bool>(
    &self,
    ctx: &OpCtx,
    promise_id: i32,
    op: impl Future<Output = R> + 'static,
    rv_map: C::MappingFn<R>,
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
    rv_map: C::MappingFn<R>,
  ) -> Option<Result<R, E>>;

  #[allow(clippy::type_complexity)]
  /// Polls the readiness of the op driver.
  fn poll_ready<'s>(
    &self,
    cx: &mut Context,
  ) -> Poll<(PromiseId, OpId, bool, OpResult<C>)>;

  fn len(&self) -> usize;
}
