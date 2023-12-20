use super::ContextState;
use crate::ops::OpCtx;
use anyhow::Error;
use smallvec::SmallVec;
use std::future::Future;
use std::task::Context;

pub mod erased_future;
pub mod joinset_driver;

pub(crate) trait OpDriver: Default {
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
  ) -> Option<R>;

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
  ) -> Option<Result<R, E>>;

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
