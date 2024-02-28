// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
use crate::error::AnyError;
use crate::op2;
use crate::CrossIsolateStore;
use crate::JsRuntime;
use crate::OpState;
use crate::RuntimeOptions;
use serde_v8::JsBuffer;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::Arc;

mod error;
mod jsrealm;
mod misc;
mod ops;
mod snapshot;

#[derive(Copy, Clone)]
pub enum Mode {
  Async,
  AsyncDeferred,
  AsyncZeroCopy(bool),
}

struct TestState {
  mode: Mode,
  dispatch_count: Arc<AtomicUsize>,
}

#[allow(clippy::await_holding_refcell_ref)] // False positive.
#[op2(async)]
async fn op_test(
  rc_op_state: Rc<RefCell<OpState>>,
  control: u8,
  #[buffer] buf: Option<JsBuffer>,
) -> Result<u8, AnyError> {
  let op_state_ = rc_op_state.borrow();
  let test_state = op_state_.borrow::<TestState>();
  test_state.dispatch_count.fetch_add(1, Ordering::Relaxed);
  let mode = test_state.mode;
  drop(op_state_);
  match mode {
    Mode::Async => {
      assert_eq!(control, 42);
      Ok(43)
    }
    Mode::AsyncDeferred => {
      tokio::task::yield_now().await;
      assert_eq!(control, 42);
      Ok(43)
    }
    Mode::AsyncZeroCopy(has_buffer) => {
      assert_eq!(buf.is_some(), has_buffer);
      if let Some(buf) = buf {
        assert_eq!(buf.len(), 1);
      }
      Ok(43)
    }
  }
}

fn setup(mode: Mode) -> (JsRuntime, Arc<AtomicUsize>) {
  let dispatch_count = Arc::new(AtomicUsize::new(0));
  deno_core::extension!(
    test_ext,
    ops = [op_test],
    options = {
      mode: Mode,
      dispatch_count: Arc<AtomicUsize>,
    },
    state = |state, options| {
      state.put(TestState {
        mode: options.mode,
        dispatch_count: options.dispatch_count
      })
    }
  );
  let mut runtime = JsRuntime::new(RuntimeOptions {
    extensions: vec![test_ext::init_ops(mode, dispatch_count.clone())],
    get_error_class_fn: Some(&|error| {
      crate::error::get_custom_error_class(error).unwrap()
    }),
    shared_array_buffer_store: Some(CrossIsolateStore::default()),
    ..Default::default()
  });

  runtime
    .execute_script_static(
      "setup.js",
      r#"
      function assert(cond) {
        if (!cond) {
          throw Error("assert");
        }
      }
      "#,
    )
    .unwrap();
  assert_eq!(dispatch_count.load(Ordering::Relaxed), 0);
  (runtime, dispatch_count)
}
