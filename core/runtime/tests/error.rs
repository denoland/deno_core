// Copyright 2018-2025 the Deno authors. MIT license.

use crate::JsRuntime;
use crate::RuntimeOptions;
use crate::error::CoreError;
use crate::op2;
use deno_error::JsErrorBox;
use std::future::poll_fn;
use std::task::Poll;

#[tokio::test]
async fn test_error_builder() {
  #[op2(fast)]
  fn op_err() -> Result<(), JsErrorBox> {
    Err(JsErrorBox::new("DOMExceptionOperationError", "abc"))
  }

  deno_core::extension!(test_ext, ops = [op_err]);
  let mut runtime = JsRuntime::new(RuntimeOptions {
    extensions: vec![test_ext::init()],
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
  let CoreError::Js(js_error) = r.unwrap_err() else {
    unreachable!()
  };
  let frame = js_error.frames.first().unwrap();
  assert_eq!(frame.column_number, Some(12));
}

#[test]
fn array_prototype_pollution_no_panic() {
  // Test for array prototype pollution bug that previously caused panics
  // This should produce a normal error, not crash the runtime
  let mut runtime = JsRuntime::new(Default::default());

  // Set up Array.prototype[0] with a getter-only property
  let setup_script = r#"
    Object.defineProperty(Array.prototype, "0", {
      get: function () {
        return "intercepted";
      },
      configurable: true
    });
    
    // Test that basic operations still work
    const testArray = [1, 2, 3];
    console.log("Array prototype pollution setup complete");
  "#;

  // This should not panic
  runtime.execute_script("setup.js", setup_script).unwrap();

  // Now trigger an error that would previously cause a panic
  let error_script = r#"
    try {
      throw new Error("Test error with prototype pollution");
    } catch (e) {
      console.log("Error caught successfully:", e.message);
    }
  "#;

  // This should complete without panicking
  let result = runtime.execute_script("error_test.js", error_script);
  assert!(
    result.is_ok(),
    "Script should execute successfully without panic"
  );
}
