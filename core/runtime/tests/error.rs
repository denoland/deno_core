// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
use crate::error::custom_error;
use crate::error::JsError;
use crate::op2;
use crate::JsRuntime;
use crate::RuntimeOptions;
use anyhow::Error;
use futures::future::poll_fn;
use std::task::Poll;

#[tokio::test]
async fn test_error_builder() {
  #[op2(fast)]
  fn op_err() -> Result<(), Error> {
    Err(custom_error("DOMExceptionOperationError", "abc"))
  }

  pub fn get_error_class_name(_: &Error) -> &'static str {
    "DOMExceptionOperationError"
  }

  deno_core::extension!(test_ext, ops = [op_err]);
  let mut runtime = JsRuntime::new(RuntimeOptions {
    extensions: vec![test_ext::init_ops()],
    get_error_class_fn: Some(&get_error_class_name),
    ..Default::default()
  });
  poll_fn(move |cx| {
    runtime
      .execute_script(
        "error_builder_test.js",
        include_str!("error_builder_test.js"),
      )
      .unwrap();
    if let Poll::Ready(Err(_)) = runtime.poll_event_loop(cx, Default::default())
    {
      unreachable!();
    }
    Poll::Ready(())
  })
  .await;
}

#[test]
fn syntax_error() {
  let mut runtime = JsRuntime::new(Default::default());
  let src = "hocuspocus(";
  let r = runtime.execute_script("i.js", src);
  let e = r.unwrap_err();
  let js_error = e.downcast::<JsError>().unwrap();
  let frame = js_error.frames.first().unwrap();
  assert_eq!(frame.column_number, Some(12));
}
