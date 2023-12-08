// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
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

#[test]
fn test_error_without_stack() {
  let mut runtime = JsRuntime::new(RuntimeOptions::default());
  // SyntaxError
  let result = runtime.execute_script_static(
    "error_without_stack.js",
    r#"
function main() {
  console.log("asdf);
}
main();
"#,
  );
  let expected_error = r#"Uncaught SyntaxError: Invalid or unexpected token
    at error_without_stack.js:3:15"#;
  assert_eq!(result.unwrap_err().to_string(), expected_error);
}

#[test]
fn test_error_stack() {
  let mut runtime = JsRuntime::new(RuntimeOptions::default());
  let result = runtime.execute_script_static(
    "error_stack.js",
    r#"
function assert(cond) {
  if (!cond) {
    throw Error("assert");
  }
}
function main() {
  assert(false);
}
main();
      "#,
  );
  let expected_error = r#"Error: assert
    at assert (error_stack.js:4:11)
    at main (error_stack.js:8:3)
    at error_stack.js:10:1"#;
  assert_eq!(result.unwrap_err().to_string(), expected_error);
}

#[tokio::test]
async fn test_error_async_stack() {
  let mut runtime = JsRuntime::new(RuntimeOptions::default());
  poll_fn(move |cx| {
    runtime
      .execute_script_static(
        "error_async_stack.js",
        r#"
  (async () => {
  const p = (async () => {
    await Promise.resolve().then(() => {
      throw new Error("async");
    });
  })();
  try {
    await p;
  } catch (error) {
    console.log(error.stack);
    throw error;
  }
  })();"#,
      )
      .unwrap();
    let expected_error = r#"Error: async
    at error_async_stack.js:5:13
    at async error_async_stack.js:4:5
    at async error_async_stack.js:9:5"#;

    match runtime.poll_event_loop(cx, Default::default()) {
      Poll::Ready(Err(e)) => {
        assert_eq!(e.to_string(), expected_error);
      }
      _ => panic!(),
    };
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

  poll_fn(move |cx| {
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
try {
  await Deno.core.opAsync("op_err_async");
} catch (err) {
  errMessage = err.message;
}
if (errMessage !== "higher-level async error: original async error") {
  throw new Error("unexpected error message from op_err_async: " + errMessage);
}
})()
"#,
      )
      .unwrap();

    match runtime.poll_value(cx, &promise) {
      Poll::Ready(Ok(_)) => {}
      Poll::Ready(Err(err)) => panic!("{err:?}"),
      _ => panic!(),
    }
    Poll::Ready(())
  })
  .await;
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
