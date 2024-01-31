// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use crate::error::custom_error;
use crate::error::JsError;
use crate::op2;
use crate::JsRuntime;
use crate::PollEventLoopOptions;
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
      .execute_script_static(
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

#[tokio::test]
async fn test_error_context() {
  use anyhow::anyhow;

  #[op2(fast)]
  fn op_err_sync() -> Result<(), Error> {
    Err(anyhow!("original sync error").context("higher-level sync error"))
  }

  #[op2(async)]
  async fn op_err_async() -> Result<(), Error> {
    Err(anyhow!("original async error").context("higher-level async error"))
  }

  deno_core::extension!(test_ext, ops = [op_err_sync, op_err_async]);
  let mut runtime = JsRuntime::new(RuntimeOptions {
    extensions: vec![test_ext::init_ops()],
    ..Default::default()
  });

  runtime
    .execute_script_static(
      "test_error_context_sync.js",
      r#"
let errMessage;
try {
  Deno.core.ops.op_err_sync();
} catch (err) {
  errMessage = err.message;
}
if (errMessage !== "higher-level sync error: original sync error") {
  throw new Error("unexpected error message from op_err_sync: " + errMessage);
}
"#,
    )
    .unwrap();

  let promise = runtime
    .execute_script_static(
      "test_error_context_async.js",
      r#"
(async () => {
  let errMessage;
  const { op_err_async } = Deno.core.ensureFastOps();
  try {
    await op_err_async();
  } catch (err) {
    errMessage = err.message;
  }
  if (errMessage !== "higher-level async error: original async error") {
    throw new Error("unexpected error message from op_err_async: " + errMessage);
  }
})()
"#,
    ).unwrap();

  let resolve = runtime.resolve(promise);
  runtime
    .with_event_loop_promise(resolve, PollEventLoopOptions::default())
    .await
    .unwrap();
}

#[test]
fn syntax_error() {
  let mut runtime = JsRuntime::new(Default::default());
  let src = "hocuspocus(";
  let r = runtime.execute_script_static("i.js", src);
  let e = r.unwrap_err();
  let js_error = e.downcast::<JsError>().unwrap();
  let frame = js_error.frames.first().unwrap();
  assert_eq!(frame.column_number, Some(12));
}

#[tokio::test]
async fn aggregate_error() {
  let mut runtime = JsRuntime::new(Default::default());
  let src = r#"
(async () => {
  await Promise.any([]);
})()
"#;
  let value_global = runtime
    .execute_script_static("test_aggregate_error.js", src)
    .unwrap();
  let resolve = runtime.resolve(value_global);
  let e = runtime
    .with_event_loop_promise(resolve, PollEventLoopOptions::default())
    .await
    .unwrap_err();
  let js_error = e.downcast::<JsError>().unwrap();

  assert_eq!(js_error.frames.len(), 0);
}

#[tokio::test]
async fn aggregate_error_destructure() {
  let mut runtime = JsRuntime::new(Default::default());
  let src = r#"
(async () => {
  try {
    await Promise.any([]);
  } catch (e) {
    return Deno.core.destructureError(e).frames;
  }
})()
"#;
  let value_global = runtime
    .execute_script_static("test_aggregate_error_destructure.js", src)
    .unwrap();
  let resolve = runtime.resolve(value_global);
  let out = runtime
    .with_event_loop_promise(resolve, PollEventLoopOptions::default())
    .await
    .unwrap();

  let scope = &mut runtime.handle_scope();

  let out = v8::Local::new(scope, out);
  let out = v8::Local::<v8::Array>::try_from(out).unwrap();

  assert_eq!(out.length(), 0);
}
