// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

use crate::error::GetErrorClassFn;
use crate::gotham_state::GothamState;
use crate::io::ResourceTable;
use crate::ops_metrics::OpMetricsFn;
use crate::runtime::JsRuntimeState;
use crate::runtime::OpDriverImpl;
use crate::FeatureChecker;
use crate::OpDecl;
use futures::task::AtomicWaker;
use std::cell::RefCell;
use std::ops::Deref;
use std::ops::DerefMut;
use std::rc::Rc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use v8::fast_api::CFunction;
use v8::Isolate;

pub type PromiseId = i32;
pub type OpId = u16;

#[cfg(debug_assertions)]
thread_local! {
  static CURRENT_OP: std::cell::Cell<Option<&'static OpDecl>> = None.into();
}

#[cfg(debug_assertions)]
pub struct ReentrancyGuard {}

#[cfg(debug_assertions)]
impl Drop for ReentrancyGuard {
  fn drop(&mut self) {
    CURRENT_OP.with(|f| f.set(None));
  }
}

/// Creates an op re-entrancy check for the given [`OpDecl`].
#[cfg(debug_assertions)]
#[doc(hidden)]
pub fn reentrancy_check(decl: &'static OpDecl) -> Option<ReentrancyGuard> {
  if decl.is_reentrant {
    return None;
  }

  let current = CURRENT_OP.with(|f| f.get());
  if let Some(current) = current {
    panic!("op {} was not marked as #[op2(reentrant)], but re-entrantly invoked op {}", current.name, decl.name);
  }
  CURRENT_OP.with(|f| f.set(Some(decl)));
  Some(ReentrancyGuard {})
}

#[derive(Clone, Copy)]
pub struct OpMetadata {
  /// A description of the op for use in sanitizer output.
  pub sanitizer_details: Option<&'static str>,
  /// The fix for the issue described in `sanitizer_details`.
  pub sanitizer_fix: Option<&'static str>,
}

impl OpMetadata {
  pub const fn default() -> Self {
    Self {
      sanitizer_details: None,
      sanitizer_fix: None,
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

  pub(crate) decl: OpDecl,
  pub(crate) fast_fn_info: Option<CFunction>,
  pub(crate) metrics_fn: Option<OpMetricsFn>,

  op_driver: Rc<OpDriverImpl>,
  runtime_state: *const JsRuntimeState,
}

impl OpCtx {
  #[allow(clippy::too_many_arguments)]
  pub(crate) fn new(
    id: OpId,
    isolate: *mut Isolate,
    op_driver: Rc<OpDriverImpl>,
    decl: OpDecl,
    state: Rc<RefCell<OpState>>,
    runtime_state: *const JsRuntimeState,
    get_error_class_fn: GetErrorClassFn,
    metrics_fn: Option<OpMetricsFn>,
  ) -> Self {
    // If we want metrics for this function, create the fastcall `CFunctionInfo` from the metrics
    // `CFunction`. For some extremely fast ops, the parameter list may change for the metrics
    // version and require a slightly different set of arguments (for example, it may need the fastcall
    // callback information to get the `OpCtx`).
    let fast_fn_info = if metrics_fn.is_some() {
      decl.fast_fn_with_metrics
    } else {
      decl.fast_fn
    };

    Self {
      id,
      state,
      get_error_class_fn,
      runtime_state,
      decl,
      op_driver,
      fast_fn_info,
      isolate,
      metrics_fn,
    }
  }

  #[inline(always)]
  pub const fn decl(&self) -> &OpDecl {
    &self.decl
  }

  #[inline(always)]
  pub const fn metrics_enabled(&self) -> bool {
    self.metrics_fn.is_some()
  }

  /// Generates four external references for each op. If an op does not have a fastcall, it generates
  /// "null" slots to avoid changing the size of the external references array.
  pub const fn external_references(&self) -> [v8::ExternalReference; 4] {
    extern "C" fn placeholder() {}

    let ctx_ptr = v8::ExternalReference {
      pointer: self as *const OpCtx as _,
    };
    let null = v8::ExternalReference {
      pointer: placeholder as _,
    };

    if self.metrics_enabled() {
      let slow_fn = v8::ExternalReference {
        function: self.decl.slow_fn_with_metrics,
      };
      if let (Some(fast_fn), Some(fast_fn_info)) =
        (self.decl.fast_fn_with_metrics, self.fast_fn_info)
      {
        let fast_fn = v8::ExternalReference {
          pointer: fast_fn.address() as _,
        };
        let fast_info = v8::ExternalReference {
          type_info: fast_fn_info.type_info(),
        };
        [ctx_ptr, slow_fn, fast_fn, fast_info]
      } else {
        [ctx_ptr, slow_fn, null, null]
      }
    } else {
      let slow_fn = v8::ExternalReference {
        function: self.decl.slow_fn,
      };
      if let (Some(fast_fn), Some(fast_fn_info)) =
        (self.decl.fast_fn, self.fast_fn_info)
      {
        let fast_fn = v8::ExternalReference {
          pointer: fast_fn.address() as _,
        };
        let fast_info = v8::ExternalReference {
          type_info: fast_fn_info.type_info(),
        };
        [ctx_ptr, slow_fn, fast_fn, fast_info]
      } else {
        [ctx_ptr, slow_fn, null, null]
      }
    }
  }

  pub(crate) fn op_driver(&self) -> &OpDriverImpl {
    &self.op_driver
  }

  /// Get the [`JsRuntimeState`] for this op.
  pub(crate) fn runtime_state(&self) -> &JsRuntimeState {
    // SAFETY: JsRuntimeState outlives OpCtx
    unsafe { &*self.runtime_state }
  }
}

/// Allows an embedder to track operations which should
/// keep the event loop alive.
#[derive(Debug, Clone)]
pub struct ExternalOpsTracker {
  counter: Arc<AtomicUsize>,
}

impl ExternalOpsTracker {
  pub fn ref_op(&self) {
    self.counter.fetch_add(1, Ordering::Relaxed);
  }

  pub fn unref_op(&self) {
    let _ =
      self
        .counter
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |x| {
          if x == 0 {
            None
          } else {
            Some(x - 1)
          }
        });
  }

  pub(crate) fn has_pending_ops(&self) -> bool {
    self.counter.load(Ordering::Relaxed) > 0
  }
}

/// Maintains the resources and ops inside a JS runtime.
pub struct OpState {
  pub resource_table: ResourceTable,
  pub(crate) gotham_state: GothamState,
  pub waker: Arc<AtomicWaker>,
  pub feature_checker: Arc<FeatureChecker>,
  pub external_ops_tracker: ExternalOpsTracker,
}

impl OpState {
  pub fn new(maybe_feature_checker: Option<Arc<FeatureChecker>>) -> OpState {
    OpState {
      resource_table: Default::default(),
      gotham_state: Default::default(),
      waker: Arc::new(AtomicWaker::new()),
      feature_checker: maybe_feature_checker.unwrap_or_default(),
      external_ops_tracker: ExternalOpsTracker {
        counter: Arc::new(AtomicUsize::new(0)),
      },
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
