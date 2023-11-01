// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.

use crate::error::AnyError;
use crate::error::GetErrorClassFn;
use crate::gotham_state::GothamState;
use crate::ops_metrics::OpMetricsFn;
use crate::resources::ResourceTable;
use crate::runtime::ContextState;
use crate::runtime::JsRuntimeState;
use crate::FeatureChecker;
use crate::OpDecl;
use anyhow::Error;
use futures::task::AtomicWaker;
use futures::Future;
use pin_project::pin_project;
use serde::Serialize;
use std::cell::RefCell;
use std::cell::UnsafeCell;
use std::ops::Deref;
use std::ops::DerefMut;
use std::ptr::NonNull;
use std::rc::Rc;
use std::rc::Weak;
use std::sync::Arc;
use v8::fast_api::CFunctionInfo;
use v8::fast_api::CTypeInfo;
use v8::Isolate;

pub type PromiseId = i32;
pub type OpId = u16;
pub struct PendingOp(pub PromiseId, pub OpId, pub OpResult, pub bool);

#[pin_project]
pub struct OpCall<F: Future<Output = OpResult>> {
  promise_id: PromiseId,
  op_id: OpId,
  /// Future is not necessarily Unpin, so we need to pin_project.
  #[pin]
  fut: F,
}

impl<F: Future<Output = OpResult>> OpCall<F> {
  /// Wraps a future; the inner future is polled the usual way (lazily).
  pub fn new(op_ctx: &OpCtx, promise_id: PromiseId, fut: F) -> Self {
    Self {
      op_id: op_ctx.id,
      promise_id,
      fut,
    }
  }
}

impl<F: Future<Output = OpResult>> Future for OpCall<F> {
  type Output = PendingOp;

  fn poll(
    self: std::pin::Pin<&mut Self>,
    cx: &mut std::task::Context<'_>,
  ) -> std::task::Poll<Self::Output> {
    let promise_id = self.promise_id;
    let op_id = self.op_id;
    let fut = self.project().fut;
    fut
      .poll(cx)
      .map(move |res| PendingOp(promise_id, op_id, res, false))
  }
}

#[allow(clippy::type_complexity)]
pub enum OpResult {
  Ok(serde_v8::SerializablePkg),
  Err(OpError),
  /// We temporarily provide a mapping function in a box for op2. This will go away when op goes away.
  Op2Temp(
    Box<
      dyn for<'a> FnOnce(
        &mut v8::HandleScope<'a>,
      )
        -> Result<v8::Local<'a, v8::Value>, serde_v8::Error>,
    >,
  ),
}

impl OpResult {
  pub fn to_v8<'a>(
    self,
    scope: &mut v8::HandleScope<'a>,
  ) -> Result<v8::Local<'a, v8::Value>, serde_v8::Error> {
    match self {
      Self::Ok(mut x) => x.to_v8(scope),
      Self::Err(err) => serde_v8::to_v8(scope, err),
      Self::Op2Temp(f) => f(scope),
    }
  }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpError {
  #[serde(rename = "$err_class_name")]
  class_name: &'static str,
  message: String,
  code: Option<&'static str>,
}

impl OpError {
  pub fn new(get_class: GetErrorClassFn, err: Error) -> Self {
    Self {
      class_name: (get_class)(&err),
      message: format!("{err:#}"),
      code: crate::error_codes::get_error_code(&err),
    }
  }
}

/// Per-op context.
///
// Note: We don't worry too much about the size of this struct because it's allocated once per realm, and is
// stored in a contiguous array.
pub struct OpCtx {
  /// The id for this op. Will be identical across realms.
  pub id: OpId,

  /// A stashed Isolate that ops can make use of. This is a raw isolate pointer, and as such, is
  /// extremely dangerous to use.
  pub isolate: *mut Isolate,

  #[doc(hidden)]
  pub state: Rc<RefCell<OpState>>,
  #[doc(hidden)]
  pub get_error_class_fn: GetErrorClassFn,

  pub(crate) decl: Rc<OpDecl>,
  pub(crate) fast_fn_c_info: Option<NonNull<v8::fast_api::CFunctionInfo>>,
  pub(crate) runtime_state: Weak<RefCell<JsRuntimeState>>,
  pub(crate) metrics_fn: Option<OpMetricsFn>,
  pub(crate) context_state: Rc<RefCell<ContextState>>,
  /// If the last fast op failed, stores the error to be picked up by the slow op.
  pub(crate) last_fast_error: UnsafeCell<Option<AnyError>>,
}

impl OpCtx {
  #[allow(clippy::too_many_arguments)]
  pub(crate) fn new(
    id: OpId,
    isolate: *mut Isolate,
    context_state: Rc<RefCell<ContextState>>,
    decl: Rc<OpDecl>,
    state: Rc<RefCell<OpState>>,
    runtime_state: Weak<RefCell<JsRuntimeState>>,
    get_error_class_fn: GetErrorClassFn,
    metrics_fn: Option<OpMetricsFn>,
  ) -> Self {
    let mut fast_fn_c_info = None;

    // If we want metrics for this function, create the fastcall `CFunctionInfo` from the metrics
    // `FastFunction`. For some extremely fast ops, the parameter list may change for the metrics
    // version and require a slightly different set of arguments (for example, it may need the fastcall
    // callback information to get the `OpCtx`).
    let fast_fn = if metrics_fn.is_some() {
      &decl.fast_fn_with_metrics
    } else {
      &decl.fast_fn
    };

    if let Some(fast_fn) = fast_fn {
      let args = CTypeInfo::new_from_slice(fast_fn.args);
      let ret = CTypeInfo::new(fast_fn.return_type);

      // SAFETY: all arguments are coming from the trait and they have
      // static lifetime
      let c_fn = unsafe {
        CFunctionInfo::new(
          args.as_ptr(),
          fast_fn.args.len(),
          ret.as_ptr(),
          fast_fn.repr,
        )
      };
      fast_fn_c_info = Some(c_fn);
    }

    Self {
      id,
      state,
      get_error_class_fn,
      runtime_state,
      decl,
      context_state,
      fast_fn_c_info,
      last_fast_error: UnsafeCell::new(None),
      isolate,
      metrics_fn,
    }
  }

  #[inline(always)]
  pub fn decl(&self) -> &OpDecl {
    &self.decl
  }

  pub fn metrics_enabled(&self) -> bool {
    self.metrics_fn.is_some()
  }

  /// This takes the last error from an [`OpCtx`], assuming that no other code anywhere
  /// can hold a `&mut` to the last_fast_error field.
  ///
  /// # Safety
  ///
  /// Must only be called from op implementations.
  #[inline(always)]
  pub unsafe fn unsafely_take_last_error_for_ops_only(
    &self,
  ) -> Option<AnyError> {
    let opt_mut = &mut *self.last_fast_error.get();
    opt_mut.take()
  }

  /// This set the last error for an [`OpCtx`], assuming that no other code anywhere
  /// can hold a `&mut` to the last_fast_error field.
  ///
  /// # Safety
  ///
  /// Must only be called from op implementations.
  #[inline(always)]
  pub unsafe fn unsafely_set_last_error_for_ops_only(&self, error: AnyError) {
    let opt_mut = &mut *self.last_fast_error.get();
    *opt_mut = Some(error);
  }
}

/// Maintains the resources and ops inside a JS runtime.
pub struct OpState {
  pub resource_table: ResourceTable,
  pub(crate) gotham_state: GothamState,
  pub waker: Arc<AtomicWaker>,
  pub feature_checker: Arc<FeatureChecker>,
}

impl OpState {
  pub fn new(maybe_feature_checker: Option<Arc<FeatureChecker>>) -> OpState {
    OpState {
      resource_table: Default::default(),
      gotham_state: Default::default(),
      waker: Arc::new(AtomicWaker::new()),
      feature_checker: maybe_feature_checker.unwrap_or_default(),
    }
  }

  /// Clear all user-provided resources and state.
  pub(crate) fn clear(&mut self) {
    std::mem::take(&mut self.gotham_state);
    std::mem::take(&mut self.resource_table);
  }
}

impl Deref for OpState {
  type Target = GothamState;

  fn deref(&self) -> &Self::Target {
    &self.gotham_state
  }
}

impl DerefMut for OpState {
  fn deref_mut(&mut self) -> &mut Self::Target {
    &mut self.gotham_state
  }
}
