// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use crate as deno_core;
use crate::extensions::Op;
use crate::extensions::OpDecl;
use crate::modules::StaticModuleLoader;
use crate::runtime::tests::setup;
use crate::runtime::tests::Mode;
use crate::*;
use anyhow::Error;
use deno_ops::op;
use log::debug;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use url::Url;

#[tokio::test]
async fn test_async_opstate_borrow() {
  struct InnerState(u64);

  #[op2(async, core)]
  async fn op_async_borrow(
    op_state: Rc<RefCell<OpState>>,
  ) -> Result<(), Error> {
    let n = {
      let op_state = op_state.borrow();
      let inner_state = op_state.borrow::<InnerState>();
      inner_state.0
    };
    // Future must be Poll::Pending on first call
    tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    if n != 42 {
      unreachable!();
    }
    Ok(())
  }

  deno_core::extension!(
    test_ext,
    ops = [op_async_borrow],
    state = |state| state.put(InnerState(42))
  );
  let mut runtime = JsRuntime::new(RuntimeOptions {
    extensions: vec![test_ext::init_ops()],
    ..Default::default()
  });

  runtime
    .execute_script_static(
      "op_async_borrow.js",
      "Deno.core.opAsync(\"op_async_borrow\")",
    )
    .unwrap();
  runtime.run_event_loop(false).await.unwrap();
}

#[tokio::test]
async fn test_sync_op_serialize_object_with_numbers_as_keys() {
  #[op2(core)]
  fn op_sync_serialize_object_with_numbers_as_keys(
    #[serde] value: serde_json::Value,
  ) -> Result<(), Error> {
    assert_eq!(
      value.to_string(),
      r#"{"lines":{"100":{"unit":"m"},"200":{"unit":"cm"}}}"#
    );
    Ok(())
  }

  deno_core::extension!(
    test_ext,
    ops = [op_sync_serialize_object_with_numbers_as_keys]
  );
  let mut runtime = JsRuntime::new(RuntimeOptions {
    extensions: vec![test_ext::init_ops()],
    ..Default::default()
  });

  runtime
    .execute_script_static(
      "op_sync_serialize_object_with_numbers_as_keys.js",
      r#"
Deno.core.ops.op_sync_serialize_object_with_numbers_as_keys({
lines: {
  100: {
    unit: "m"
  },
  200: {
    unit: "cm"
  }
}
})
"#,
    )
    .unwrap();
  runtime.run_event_loop(false).await.unwrap();
}

#[tokio::test]
async fn test_async_op_serialize_object_with_numbers_as_keys() {
  #[op2(async, core)]
  async fn op_async_serialize_object_with_numbers_as_keys(
    #[serde] value: serde_json::Value,
  ) -> Result<(), Error> {
    assert_eq!(
      value.to_string(),
      r#"{"lines":{"100":{"unit":"m"},"200":{"unit":"cm"}}}"#
    );
    Ok(())
  }

  deno_core::extension!(
    test_ext,
    ops = [op_async_serialize_object_with_numbers_as_keys]
  );
  let mut runtime = JsRuntime::new(RuntimeOptions {
    extensions: vec![test_ext::init_ops()],
    ..Default::default()
  });

  runtime
    .execute_script_static(
      "op_async_serialize_object_with_numbers_as_keys.js",
      r#"

Deno.core.opAsync("op_async_serialize_object_with_numbers_as_keys", {
lines: {
  100: {
    unit: "m"
  },
  200: {
    unit: "cm"
  }
}
})
"#,
    )
    .unwrap();
  runtime.run_event_loop(false).await.unwrap();
}

#[test]
fn test_op_return_serde_v8_error() {
  #[op2(core)]
  #[serde]
  fn op_err() -> Result<std::collections::BTreeMap<u64, u64>, anyhow::Error> {
    Ok([(1, 2), (3, 4)].into_iter().collect()) // Maps can't have non-string keys in serde_v8
  }

  deno_core::extension!(test_ext, ops = [op_err]);
  let mut runtime = JsRuntime::new(RuntimeOptions {
    extensions: vec![test_ext::init_ops()],
    ..Default::default()
  });
  assert!(runtime
    .execute_script_static(
      "test_op_return_serde_v8_error.js",
      "Deno.core.ops.op_err()"
    )
    .is_err());
}

#[test]
fn test_op_high_arity() {
  #[op]
  fn op_add_4(
    x1: i64,
    x2: i64,
    x3: i64,
    x4: i64,
  ) -> Result<i64, anyhow::Error> {
    Ok(x1 + x2 + x3 + x4)
  }

  deno_core::extension!(test_ext, ops = [op_add_4]);
  let mut runtime = JsRuntime::new(RuntimeOptions {
    extensions: vec![test_ext::init_ops()],
    ..Default::default()
  });
  let r = runtime
    .execute_script_static("test.js", "Deno.core.ops.op_add_4(1, 2, 3, 4)")
    .unwrap();
  let scope = &mut runtime.handle_scope();
  assert_eq!(r.open(scope).integer_value(scope), Some(10));
}

#[test]
fn test_op_disabled() {
  #[op]
  fn op_foo() -> Result<i64, anyhow::Error> {
    Ok(42)
  }

  fn ops() -> Vec<OpDecl> {
    vec![op_foo::DECL.disable()]
  }

  deno_core::extension!(test_ext, ops_fn = ops);
  let mut runtime = JsRuntime::new(RuntimeOptions {
    extensions: vec![test_ext::init_ops()],
    ..Default::default()
  });
  let err = runtime
    .execute_script_static("test.js", "Deno.core.ops.op_foo()")
    .unwrap_err();
  assert!(err
    .to_string()
    .contains("TypeError: Deno.core.ops.op_foo is not a function"));
}

#[test]
fn test_op_detached_buffer() {
  #[op2(core)]
  fn op_sum_take(#[buffer(detach)] b: JsBuffer) -> Result<u32, anyhow::Error> {
    Ok(b.as_ref().iter().clone().map(|x| *x as u32).sum())
  }

  #[op2(core)]
  #[buffer]
  fn op_boomerang(
    #[buffer(detach)] b: JsBuffer,
  ) -> Result<JsBuffer, anyhow::Error> {
    Ok(b)
  }

  deno_core::extension!(test_ext, ops = [op_sum_take, op_boomerang]);
  let mut runtime = JsRuntime::new(RuntimeOptions {
    extensions: vec![test_ext::init_ops()],
    ..Default::default()
  });

  runtime
    .execute_script_static(
      "test.js",
      r#"
      const a1 = new Uint8Array([1,2,3]);
      const a1b = a1.subarray(0, 3);
      const a2 = new Uint8Array([5,10,15]);
      const a2b = a2.subarray(0, 3);
      if (!(a1.length > 0 && a1b.length > 0)) {
        throw new Error("a1 & a1b should have a length");
      }
      let sum = Deno.core.ops.op_sum_take(a1b);
      if (sum !== 6) {
        throw new Error(`Bad sum: ${sum}`);
      }
      if (a1.length > 0 || a1b.length > 0) {
        throw new Error("expecting a1 & a1b to be detached");
      }
      const a3 = Deno.core.ops.op_boomerang(a2b);
      if (a3.byteLength != 3) {
        throw new Error(`Expected a3.byteLength === 3, got ${a3.byteLength}`);
      }
      if (a3[0] !== 5 || a3[1] !== 10) {
        throw new Error(`Invalid a3: ${a3[0]}, ${a3[1]}`);
      }
      if (a2.byteLength > 0 || a2b.byteLength > 0) {
        throw new Error("expecting a2 & a2b to be detached, a3 re-attached");
      }
      "#,
    )
    .unwrap();

  runtime
    .execute_script_static(
      "test.js",
      r#"
      const wmem = new WebAssembly.Memory({ initial: 1, maximum: 2 });
      const w32 = new Uint32Array(wmem.buffer);
      w32[0] = 1; w32[1] = 2; w32[2] = 3;
      const assertWasmThrow = (() => {
        try {
          let sum = Deno.core.ops.op_sum_take(w32.subarray(0, 2));
          return false;
        } catch(e) {
          return e.message.includes('expected');
        }
      });
      if (!assertWasmThrow()) {
        throw new Error("expected wasm mem to not be detachable");
      }
    "#,
    )
    .unwrap();
}

#[test]
fn test_op_unstable_disabling() {
  #[op]
  fn op_foo() -> Result<i64, anyhow::Error> {
    Ok(42)
  }

  #[op(unstable)]
  fn op_bar() -> Result<i64, anyhow::Error> {
    Ok(42)
  }

  deno_core::extension!(
    test_ext,
    ops = [op_foo, op_bar],
    middleware = |op| if op.is_unstable { op.disable() } else { op }
  );
  let mut runtime = JsRuntime::new(RuntimeOptions {
    extensions: vec![test_ext::init_ops()],
    ..Default::default()
  });
  runtime
    .execute_script_static(
      "test.js",
      r#"
      if (Deno.core.ops.op_foo() !== 42) {
        throw new Error("Expected op_foo() === 42");
      }
      if (typeof Deno.core.ops.op_bar !== "undefined") {
        throw new Error("Expected op_bar to be disabled")
      }
    "#,
    )
    .unwrap();
}

#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "Found ops with duplicate names:")]
fn duplicate_op_names() {
  mod a {
    use super::*;

    #[op2(core)]
    #[string]
    pub fn op_test() -> Result<String, Error> {
      Ok(String::from("Test"))
    }
  }

  #[op2(core)]
  #[string]
  pub fn op_test() -> Result<String, Error> {
    Ok(String::from("Test"))
  }

  deno_core::extension!(test_ext, ops = [a::op_test, op_test]);
  JsRuntime::new(RuntimeOptions {
    extensions: vec![test_ext::init_ops()],
    ..Default::default()
  });
}

#[test]
fn ops_in_js_have_proper_names() {
  #[op2(core)]
  #[string]
  fn op_test_sync() -> Result<String, Error> {
    Ok(String::from("Test"))
  }

  #[op]
  async fn op_test_async() -> Result<String, Error> {
    Ok(String::from("Test"))
  }

  deno_core::extension!(test_ext, ops = [op_test_sync, op_test_async]);
  let mut runtime = JsRuntime::new(RuntimeOptions {
    extensions: vec![test_ext::init_ops()],
    ..Default::default()
  });

  let src = r#"
  if (Deno.core.ops.op_test_sync.name !== "op_test_sync") {
    throw new Error();
  }

  if (Deno.core.ops.op_test_async.name !== "op_test_async") {
    throw new Error();
  }

  const { op_test_async } = Deno.core.ensureFastOps();
  if (op_test_async.name !== "op_test_async") {
    throw new Error();
  }
  "#;
  runtime.execute_script_static("test", src).unwrap();
}

#[tokio::test]
async fn test_ref_unref_ops() {
  let (mut runtime, _dispatch_count) = setup(Mode::AsyncDeferred);
  runtime
    .execute_script_static(
      "filename.js",
      r#"

      var promiseIdSymbol = Symbol.for("Deno.core.internalPromiseId");
      var p1 = Deno.core.opAsync("op_test", 42);
      var p2 = Deno.core.opAsync("op_test", 42);
      "#,
    )
    .unwrap();
  {
    let realm = runtime.main_realm();
    assert_eq!(realm.num_pending_ops(), 2);
    assert_eq!(realm.num_unrefed_ops(), 0);
  }
  runtime
    .execute_script_static(
      "filename.js",
      r#"
      Deno.core.ops.op_unref_op(p1[promiseIdSymbol]);
      Deno.core.ops.op_unref_op(p2[promiseIdSymbol]);
      "#,
    )
    .unwrap();
  {
    let realm = runtime.main_realm();
    assert_eq!(realm.num_pending_ops(), 2);
    assert_eq!(realm.num_unrefed_ops(), 2);
  }
  runtime
    .execute_script_static(
      "filename.js",
      r#"
      Deno.core.ops.op_ref_op(p1[promiseIdSymbol]);
      Deno.core.ops.op_ref_op(p2[promiseIdSymbol]);
      "#,
    )
    .unwrap();
  {
    let realm = runtime.main_realm();
    assert_eq!(realm.num_pending_ops(), 2);
    assert_eq!(realm.num_unrefed_ops(), 0);
  }
}

#[test]
fn test_dispatch() {
  let (mut runtime, dispatch_count) = setup(Mode::Async);
  runtime
    .execute_script_static(
      "filename.js",
      r#"
      let control = 42;

      Deno.core.opAsync("op_test", control);
      async function main() {
        Deno.core.opAsync("op_test", control);
      }
      main();
      "#,
    )
    .unwrap();
  assert_eq!(dispatch_count.load(Ordering::Relaxed), 2);
}

#[test]
fn test_op_async_promise_id() {
  let (mut runtime, _dispatch_count) = setup(Mode::Async);
  runtime
    .execute_script_static(
      "filename.js",
      r#"

      const p = Deno.core.opAsync("op_test", 42);
      if (p[Symbol.for("Deno.core.internalPromiseId")] == undefined) {
        throw new Error("missing id on returned promise");
      }
      "#,
    )
    .unwrap();
}

#[test]
fn test_dispatch_no_zero_copy_buf() {
  let (mut runtime, dispatch_count) = setup(Mode::AsyncZeroCopy(false));
  runtime
    .execute_script_static(
      "filename.js",
      r#"

      Deno.core.opAsync("op_test", 0);
      "#,
    )
    .unwrap();
  assert_eq!(dispatch_count.load(Ordering::Relaxed), 1);
}

#[test]
fn test_dispatch_stack_zero_copy_bufs() {
  let (mut runtime, dispatch_count) = setup(Mode::AsyncZeroCopy(true));
  runtime
    .execute_script_static(
      "filename.js",
      r#"
      const { op_test } = Deno.core.ensureFastOps();
      let zero_copy_a = new Uint8Array([0]);
      op_test(0, zero_copy_a);
      "#,
    )
    .unwrap();
  assert_eq!(dispatch_count.load(Ordering::Relaxed), 1);
}

/// Test that long-running ops do not block dynamic imports from loading.
// https://github.com/denoland/deno/issues/19903
// https://github.com/denoland/deno/issues/19455
#[tokio::test]
pub async fn test_top_level_await() {
  #[op2(async)]
  async fn op_sleep_forever() {
    tokio::time::sleep(Duration::MAX).await
  }

  deno_core::extension!(testing, ops = [op_sleep_forever]);

  let loader = StaticModuleLoader::new([
    (
      Url::parse("http://x/main.js").unwrap(),
      ascii_str!(
        r#"
const { op_sleep_forever } = Deno.core.ensureFastOps();
(async () => await op_sleep_forever())();
await import('./mod.js');
    "#
      ),
    ),
    (
      Url::parse("http://x/mod.js").unwrap(),
      ascii_str!(
        r#"
const { op_void_async_deferred } = Deno.core.ensureFastOps();
await op_void_async_deferred();
    "#
      ),
    ),
  ]);

  let mut runtime = JsRuntime::new(RuntimeOptions {
    module_loader: Some(Rc::new(loader)),
    extensions: vec![testing::init_ops()],
    ..Default::default()
  });

  let id = runtime
    .load_main_module(&Url::parse("http://x/main.js").unwrap(), None)
    .await
    .unwrap();
  let mut rx = runtime.mod_evaluate(id);

  let res = tokio::select! {
    // Not using biased mode leads to non-determinism for relatively simple
    // programs.
    biased;

    maybe_result = &mut rx => {
      debug!("received module evaluate {:#?}", maybe_result);
      maybe_result.expect("Module evaluation result not provided.")
    }

    event_loop_result = runtime.run_event_loop(false) => {
      event_loop_result.unwrap();
      let maybe_result = rx.await;
      maybe_result.expect("Module evaluation result not provided.")
    }
  };
  res.unwrap();
}
