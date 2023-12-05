// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use crate::error::exception_to_err_result;
use anyhow::Error;
use std::cell::Cell;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

#[derive(Default)]
pub(crate) struct ExceptionState {
  // TODO(nayeemrmn): This is polled in `exception_to_err_result()` which is
  // flimsy. Try to poll it similarly to `pending_promise_rejections`.
  dispatched_exception: Cell<Option<v8::Global<v8::Value>>>,
  dispatched_exception_is_promise: Cell<bool>,
  pub(crate) pending_promise_rejections:
    RefCell<VecDeque<(v8::Global<v8::Promise>, v8::Global<v8::Value>)>>,
  pub(crate) js_build_custom_error_cb:
    RefCell<Option<Rc<v8::Global<v8::Function>>>>,
  pub(crate) js_promise_reject_cb:
    RefCell<Option<Rc<v8::Global<v8::Function>>>>,
  pub(crate) js_format_exception_cb:
    RefCell<Option<Rc<v8::Global<v8::Function>>>>,
}

impl ExceptionState {
  /// Clear all the associated v8 objects to prepare for this isolate to be torn down, either for
  /// a snapshot or for process termination purposes.
  ///
  /// The [`ExceptionState`] is not considered valid after this operation and should not be used.
  /// It generally will not live long after this, however.
  pub(crate) fn prepare_to_destroy(&self) {
    // TODO(mmastrac): we can probably move this to Drop eventually
    self.js_build_custom_error_cb.borrow_mut().take();
    self.js_promise_reject_cb.borrow_mut().take();
    self.js_format_exception_cb.borrow_mut().take();
    self.pending_promise_rejections.borrow_mut().clear();
    self.dispatched_exception.set(None);
  }

  pub(crate) fn clear_error(&self) {
    self.dispatched_exception_is_promise.set(false);
    self.dispatched_exception.set(None);
  }

  pub(crate) fn has_dispatched_exception(&self) -> bool {
    // SAFETY: we limit access to this cell to this method only
    unsafe {
      self
        .dispatched_exception
        .as_ptr()
        .as_ref()
        .unwrap_unchecked()
        .is_some()
    }
  }

  pub(crate) fn set_dispatched_exception(
    &self,
    exception: v8::Global<v8::Value>,
    promise: bool,
  ) {
    self.dispatched_exception.set(Some(exception));
    self.dispatched_exception_is_promise.set(promise);
  }

  /// If there is an exception condition (ie: an unhandled promise rejection or exception, or
  /// the runtime is shut down), returns it from here. If not, returns `Ok`.
  pub(crate) fn check_exception_condition(
    &self,
    scope: &mut v8::HandleScope,
  ) -> Result<(), Error> {
    if self.has_dispatched_exception() {
      let undefined = v8::undefined(scope);
      exception_to_err_result(
        scope,
        undefined.into(),
        self.dispatched_exception_is_promise.get(),
        true,
      )
    } else {
      Ok(())
    }
  }

  pub(crate) fn is_dispatched_exception_promise(&self) -> bool {
    self.dispatched_exception_is_promise.get()
  }

  pub(crate) fn get_dispatched_exception_as_local<'s>(
    &self,
    scope: &mut v8::HandleScope<'s>,
  ) -> Option<v8::Local<'s, v8::Value>> {
    // SAFETY: we limit access to this cell to this method only
    unsafe {
      self
        .dispatched_exception
        .as_ptr()
        .as_ref()
        .unwrap_unchecked()
    }
    .as_ref()
    .map(|global| v8::Local::new(scope, global))
  }

  /// Tracks this promise rejection until we have a chance to give it to the
  /// unhandled promise rejection handler.
  pub fn track_promise_rejection(
    &self,
    scope: &mut v8::HandleScope,
    promise: v8::Local<v8::Promise>,
    event: v8::PromiseRejectEvent,
    rejection_value: Option<v8::Local<v8::Value>>,
  ) {
    use v8::PromiseRejectEvent::*;
    let promise_global = v8::Global::new(scope, promise);
    match event {
      PromiseRejectWithNoHandler => {
        let error = rejection_value.unwrap();
        let error_global = v8::Global::new(scope, error);
        self
          .pending_promise_rejections
          .borrow_mut()
          .push_back((promise_global, error_global));
      }
      PromiseHandlerAddedAfterReject => {
        self
          .pending_promise_rejections
          .borrow_mut()
          .retain(|(key, _)| key != &promise_global);
      }
      PromiseRejectAfterResolved => {}
      PromiseResolveAfterResolved => {
        // Should not warn. See #1272
      }
    }
  }
}
