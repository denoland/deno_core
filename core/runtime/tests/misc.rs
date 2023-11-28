// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use crate::error::AnyError;
use crate::error::JsError;
use crate::modules::StaticModuleLoader;
use crate::runtime::tests::setup;
use crate::runtime::tests::Mode;
use crate::*;
use anyhow::Error;
use cooked_waker::IntoWaker;
use cooked_waker::Wake;
use cooked_waker::WakeRef;
use futures::future::poll_fn;
use std::borrow::Cow;
use std::rc::Rc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicI8;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::task::Context;
use std::task::Poll;
use std::time::Duration;
use url::Url;

#[test]
fn icu() {
  // If this test fails, update core/runtime/icudtl.dat from
  // rusty_v8/third_party/icu/common/icudtl.dat
  let mut runtime = JsRuntime::new(Default::default());
  runtime
    .execute_script_static("a.js", "(new Date()).toLocaleString('ja-JP')")
    .unwrap();
}

#[test]
fn test_execute_script_return_value() {
  let mut runtime = JsRuntime::new(Default::default());
  let value_global =
    runtime.execute_script_static("a.js", "a = 1 + 2").unwrap();
  {
    let scope = &mut runtime.handle_scope();
    let value = value_global.open(scope);
    assert_eq!(value.integer_value(scope).unwrap(), 3);
  }
  let value_global = runtime
    .execute_script_static("b.js", "b = 'foobar'")
    .unwrap();
  {
    let scope = &mut runtime.handle_scope();
    let value = value_global.open(scope);
    assert!(value.is_string());
    assert_eq!(
      value.to_string(scope).unwrap().to_rust_string_lossy(scope),
      "foobar"
    );
  }
}

#[derive(Default)]
struct LoggingWaker {
  woken: AtomicBool,
}

impl Wake for LoggingWaker {
  fn wake(self) {
    self.woken.store(true, Ordering::SeqCst);
  }
}

impl WakeRef for LoggingWaker {
  fn wake_by_ref(&self) {
    self.woken.store(true, Ordering::SeqCst);
  }
}

/// This is a reproduction for a very obscure bug where the Deno runtime locks up we end up polling
/// an empty JoinSet and attempt to resolve ops after-the-fact. There's a small footgun in the JoinSet
/// API where polling it while empty returns Ready(None), which means that it never holds on to the
/// waker. This means that if we aren't testing for this particular return value and don't stash the waker
/// ourselves for a future async op to eventually queue, we can end up losing the waker entirely and the
/// op wakes up, notifies tokio, which notifies the JoinSet, which then has nobody to notify )`:.
#[tokio::test]
async fn test_wakers_for_async_ops() {
  static STATE: AtomicI8 = AtomicI8::new(0);

  #[op2(async)]
  async fn op_async_sleep() -> Result<(), Error> {
    STATE.store(1, Ordering::SeqCst);
    tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    STATE.store(2, Ordering::SeqCst);
    Ok(())
  }

  STATE.store(0, Ordering::SeqCst);

  let logging_waker = Arc::new(LoggingWaker::default());
  let waker = logging_waker.clone().into_waker();

  deno_core::extension!(test_ext, ops = [op_async_sleep]);
  let mut runtime = JsRuntime::new(RuntimeOptions {
    extensions: vec![test_ext::init_ops()],
    ..Default::default()
  });

  // Drain events until we get to Ready
  loop {
    logging_waker.woken.store(false, Ordering::SeqCst);
    let res = runtime.poll_event_loop(&mut Context::from_waker(&waker), false);
    let ready = matches!(res, Poll::Ready(Ok(())));
    assert!(ready || logging_waker.woken.load(Ordering::SeqCst));
    if ready {
      break;
    }
  }

  // Start the AIIFE
  runtime
    .execute_script(
      "",
      FastString::from_static(
        "(async () => { await Deno.core.opAsync('op_async_sleep'); })()",
      ),
    )
    .unwrap();

  // Wait for future to finish
  while STATE.load(Ordering::SeqCst) < 2 {
    tokio::time::sleep(Duration::from_millis(1)).await;
  }

  // This shouldn't take one minute, but if it does, things are definitely locked up
  for _ in 0..Duration::from_secs(60).as_millis() {
    if logging_waker.woken.load(Ordering::SeqCst) {
      // Success
      return;
    }
    tokio::time::sleep(Duration::from_millis(1)).await;
  }

  panic!("The waker was never woken after the future completed");
}

#[tokio::test]
async fn test_poll_value() {
  let mut runtime = JsRuntime::new(Default::default());
  poll_fn(move |cx| {
    let value_global = runtime
      .execute_script_static("a.js", "Promise.resolve(1 + 2)")
      .unwrap();
    let v = runtime.poll_value(cx, &value_global);
    {
      let scope = &mut runtime.handle_scope();
      assert!(
        matches!(v, Poll::Ready(Ok(v)) if v.open(scope).integer_value(scope).unwrap() == 3)
      );
    }

    let value_global = runtime
      .execute_script_static(
        "a.js",
        "Promise.resolve(new Promise(resolve => resolve(2 + 2)))",
      )
      .unwrap();
    let v = runtime.poll_value(cx, &value_global);
    {
      let scope = &mut runtime.handle_scope();
      assert!(
        matches!(v, Poll::Ready(Ok(v)) if v.open(scope).integer_value(scope).unwrap() == 4)
      );
    }

    let value_global = runtime
      .execute_script_static("a.js", "Promise.reject(new Error('fail'))")
      .unwrap();
    let v = runtime.poll_value(cx, &value_global);
    assert!(
      matches!(v, Poll::Ready(Err(e)) if e.downcast_ref::<JsError>().unwrap().exception_message == "Uncaught Error: fail")
    );

    let value_global = runtime
      .execute_script_static("a.js", "new Promise(resolve => {})")
      .unwrap();
    let v = runtime.poll_value(cx, &value_global);
    matches!(v, Poll::Ready(Err(e)) if e.to_string() == "Promise resolution is still pending but the event loop has already resolved.");
    Poll::Ready(())
  }).await;
}

#[tokio::test]
async fn test_resolve_value() {
  let mut runtime = JsRuntime::new(Default::default());
  let value_global = runtime
    .execute_script_static("a.js", "Promise.resolve(1 + 2)")
    .unwrap();
  let result_global = runtime.resolve_value(value_global).await.unwrap();
  {
    let scope = &mut runtime.handle_scope();
    let value = result_global.open(scope);
    assert_eq!(value.integer_value(scope).unwrap(), 3);
  }

  let value_global = runtime
    .execute_script_static(
      "a.js",
      "Promise.resolve(new Promise(resolve => resolve(2 + 2)))",
    )
    .unwrap();
  let result_global = runtime.resolve_value(value_global).await.unwrap();
  {
    let scope = &mut runtime.handle_scope();
    let value = result_global.open(scope);
    assert_eq!(value.integer_value(scope).unwrap(), 4);
  }

  let value_global = runtime
    .execute_script_static("a.js", "Promise.reject(new Error('fail'))")
    .unwrap();
  let err = runtime.resolve_value(value_global).await.unwrap_err();
  assert_eq!(
    "Uncaught Error: fail",
    err.downcast::<JsError>().unwrap().exception_message
  );

  let value_global = runtime
    .execute_script_static("a.js", "new Promise(resolve => {})")
    .unwrap();
  let error_string = runtime
    .resolve_value(value_global)
    .await
    .unwrap_err()
    .to_string();
  assert_eq!(
    "Promise resolution is still pending but the event loop has already resolved.",
    error_string,
  );
}

#[test]
fn terminate_execution_webassembly() {
  let (mut runtime, _dispatch_count) = setup(Mode::Async);
  let v8_isolate_handle = runtime.v8_isolate().thread_safe_handle();

  // Run an infinite loop in WebAssembly code, which should be terminated.
  let promise = runtime.execute_script_static("infinite_wasm_loop.js",
                               r#"
                               (async () => {
                                const wasmCode = new Uint8Array([
                                    0,    97,   115,  109,  1,    0,    0,    0,    1,   4,    1,
                                    96,   0,    0,    3,    2,    1,    0,    7,    17,  1,    13,
                                    105,  110,  102,  105,  110,  105,  116,  101,  95,  108,  111,
                                    111,  112,  0,    0,    10,   9,    1,    7,    0,   3,    64,
                                    12,   0,    11,   11,
                                ]);
                                const wasmModule = await WebAssembly.compile(wasmCode);
                                globalThis.wasmInstance = new WebAssembly.Instance(wasmModule);
                                })()
                                    "#).unwrap();
  futures::executor::block_on(runtime.resolve_value(promise)).unwrap();
  let terminator_thread = std::thread::spawn(move || {
    std::thread::sleep(std::time::Duration::from_millis(1000));

    // terminate execution
    let ok = v8_isolate_handle.terminate_execution();
    assert!(ok);
  });
  let err = runtime
    .execute_script_static(
      "infinite_wasm_loop2.js",
      "globalThis.wasmInstance.exports.infinite_loop();",
    )
    .unwrap_err();
  assert_eq!(err.to_string(), "Uncaught Error: execution terminated");
  // Cancel the execution-terminating exception in order to allow script
  // execution again.
  let ok = runtime.v8_isolate().cancel_terminate_execution();
  assert!(ok);

  // Verify that the isolate usable again.
  runtime
    .execute_script_static("simple.js", "1 + 1")
    .expect("execution should be possible again");

  terminator_thread.join().unwrap();
}

#[test]
fn terminate_execution() {
  let (mut isolate, _dispatch_count) = setup(Mode::Async);
  let v8_isolate_handle = isolate.v8_isolate().thread_safe_handle();

  let terminator_thread = std::thread::spawn(move || {
    // allow deno to boot and run
    std::thread::sleep(std::time::Duration::from_millis(100));

    // terminate execution
    let ok = v8_isolate_handle.terminate_execution();
    assert!(ok);
  });

  // Rn an infinite loop, which should be terminated.
  match isolate.execute_script_static("infinite_loop.js", "for(;;) {}") {
    Ok(_) => panic!("execution should be terminated"),
    Err(e) => {
      assert_eq!(e.to_string(), "Uncaught Error: execution terminated")
    }
  };

  // Cancel the execution-terminating exception in order to allow script
  // execution again.
  let ok = isolate.v8_isolate().cancel_terminate_execution();
  assert!(ok);

  // Verify that the isolate usable again.
  isolate
    .execute_script_static("simple.js", "1 + 1")
    .expect("execution should be possible again");

  terminator_thread.join().unwrap();
}

#[tokio::test]
async fn wasm_streaming_op_invocation_in_import() {
  let (mut runtime, _dispatch_count) = setup(Mode::Async);

  // Run an infinite loop in WebAssembly code, which should be terminated.
  runtime.execute_script_static("setup.js",
                               r#"
                                Deno.core.setWasmStreamingCallback((source, rid) => {
                                  Deno.core.ops.op_wasm_streaming_set_url(rid, "file:///foo.wasm");
                                  Deno.core.ops.op_wasm_streaming_feed(rid, source);
                                  Deno.core.close(rid);
                                });
                               "#).unwrap();

  let promise = runtime.execute_script_static("main.js",
                            r#"
                             // (module (import "env" "data" (global i64)))
                             const bytes = new Uint8Array([0,97,115,109,1,0,0,0,2,13,1,3,101,110,118,4,100,97,116,97,3,126,0,0,8,4,110,97,109,101,2,1,0]);
                             WebAssembly.instantiateStreaming(bytes, {
                               env: {
                                 get data() {
                                   return new WebAssembly.Global({ value: "i64", mutable: false }, 42n);
                                 }
                               }
                             });
                            "#).unwrap();
  let value = runtime.resolve_value(promise).await.unwrap();
  let val = value.open(&mut runtime.handle_scope());
  assert!(val.is_object());
}

#[test]
fn dangling_shared_isolate() {
  let v8_isolate_handle = {
    // isolate is dropped at the end of this block
    let (mut runtime, _dispatch_count) = setup(Mode::Async);
    runtime.v8_isolate().thread_safe_handle()
  };

  // this should not SEGFAULT
  v8_isolate_handle.terminate_execution();
}

#[tokio::test]
async fn test_encode_decode() {
  let (mut runtime, _dispatch_count) = setup(Mode::Async);
  poll_fn(move |cx| {
    runtime
      .execute_script(
        "encode_decode_test.js",
        // Note: We make this to_owned because it contains non-ASCII chars
        include_str!("encode_decode_test.js").to_owned().into(),
      )
      .unwrap();
    if let Poll::Ready(Err(_)) = runtime.poll_event_loop(cx, false) {
      unreachable!();
    }
    Poll::Ready(())
  })
  .await;
}

#[tokio::test]
async fn test_serialize_deserialize() {
  let (mut runtime, _dispatch_count) = setup(Mode::Async);
  poll_fn(move |cx| {
    runtime
      .execute_script(
        "serialize_deserialize_test.js",
        include_ascii_string!("serialize_deserialize_test.js"),
      )
      .unwrap();
    if let Poll::Ready(Err(_)) = runtime.poll_event_loop(cx, false) {
      unreachable!();
    }
    Poll::Ready(())
  })
  .await;
}

/// Ensure that putting the inspector into OpState doesn't cause crashes. The only valid place we currently allow
/// the inspector to be stashed without cleanup is the OpState, and this should not actually cause crashes.
#[test]
fn inspector() {
  let mut runtime = JsRuntime::new(RuntimeOptions {
    inspector: true,
    ..Default::default()
  });
  // This was causing a crash
  runtime.op_state().borrow_mut().put(runtime.inspector());
  runtime.execute_script_static("check.js", "null").unwrap();
}

#[test]
fn test_get_module_namespace() {
  let mut runtime = JsRuntime::new(RuntimeOptions {
    module_loader: Some(Rc::new(NoopModuleLoader)),
    ..Default::default()
  });

  let specifier = crate::resolve_url("file:///main.js").unwrap();
  let source_code = ascii_str!(
    r#"
    export const a = "b";
    export default 1 + 2;
    "#
  );

  let module_id = futures::executor::block_on(
    runtime.load_main_module(&specifier, Some(source_code)),
  )
  .unwrap();

  #[allow(clippy::let_underscore_future)]
  let _ = runtime.mod_evaluate(module_id);

  let module_namespace = runtime.get_module_namespace(module_id).unwrap();

  let scope = &mut runtime.handle_scope();

  let module_namespace = v8::Local::<v8::Object>::new(scope, module_namespace);

  assert!(module_namespace.is_module_namespace_object());

  let unknown_export_name = v8::String::new(scope, "none").unwrap();
  let binding = module_namespace.get(scope, unknown_export_name.into());

  assert!(binding.is_some());
  assert!(binding.unwrap().is_undefined());

  let empty_export_name = v8::String::new(scope, "").unwrap();
  let binding = module_namespace.get(scope, empty_export_name.into());

  assert!(binding.is_some());
  assert!(binding.unwrap().is_undefined());

  let a_export_name = v8::String::new(scope, "a").unwrap();
  let binding = module_namespace.get(scope, a_export_name.into());

  assert!(binding.unwrap().is_string());
  assert_eq!(binding.unwrap(), v8::String::new(scope, "b").unwrap());

  let default_export_name = v8::String::new(scope, "default").unwrap();
  let binding = module_namespace.get(scope, default_export_name.into());

  assert!(binding.unwrap().is_number());
  assert_eq!(binding.unwrap(), v8::Number::new(scope, 3_f64));
}

#[test]
fn test_heap_limits() {
  let create_params =
    v8::Isolate::create_params().heap_limits(0, 5 * 1024 * 1024);
  let mut runtime = JsRuntime::new(RuntimeOptions {
    create_params: Some(create_params),
    ..Default::default()
  });
  let cb_handle = runtime.v8_isolate().thread_safe_handle();

  let callback_invoke_count = Rc::new(AtomicUsize::new(0));
  let inner_invoke_count = Rc::clone(&callback_invoke_count);

  runtime.add_near_heap_limit_callback(move |current_limit, _initial_limit| {
    inner_invoke_count.fetch_add(1, Ordering::SeqCst);
    cb_handle.terminate_execution();
    current_limit * 2
  });
  let err = runtime
    .execute_script_static(
      "script name",
      r#"let s = ""; while(true) { s += "Hello"; }"#,
    )
    .expect_err("script should fail");
  assert_eq!(
    "Uncaught Error: execution terminated",
    err.downcast::<JsError>().unwrap().exception_message
  );
  assert!(callback_invoke_count.load(Ordering::SeqCst) > 0)
}

#[test]
fn test_heap_limit_cb_remove() {
  let mut runtime = JsRuntime::new(Default::default());

  runtime.add_near_heap_limit_callback(|current_limit, _initial_limit| {
    current_limit * 2
  });
  runtime.remove_near_heap_limit_callback(3 * 1024 * 1024);
  assert!(runtime.allocations.near_heap_limit_callback_data.is_none());
}

#[test]
fn test_heap_limit_cb_multiple() {
  let create_params =
    v8::Isolate::create_params().heap_limits(0, 5 * 1024 * 1024);
  let mut runtime = JsRuntime::new(RuntimeOptions {
    create_params: Some(create_params),
    ..Default::default()
  });
  let cb_handle = runtime.v8_isolate().thread_safe_handle();

  let callback_invoke_count_first = Rc::new(AtomicUsize::new(0));
  let inner_invoke_count_first = Rc::clone(&callback_invoke_count_first);
  runtime.add_near_heap_limit_callback(move |current_limit, _initial_limit| {
    inner_invoke_count_first.fetch_add(1, Ordering::SeqCst);
    current_limit * 2
  });

  let callback_invoke_count_second = Rc::new(AtomicUsize::new(0));
  let inner_invoke_count_second = Rc::clone(&callback_invoke_count_second);
  runtime.add_near_heap_limit_callback(move |current_limit, _initial_limit| {
    inner_invoke_count_second.fetch_add(1, Ordering::SeqCst);
    cb_handle.terminate_execution();
    current_limit * 2
  });

  let err = runtime
    .execute_script_static(
      "script name",
      r#"let s = ""; while(true) { s += "Hello"; }"#,
    )
    .expect_err("script should fail");
  assert_eq!(
    "Uncaught Error: execution terminated",
    err.downcast::<JsError>().unwrap().exception_message
  );
  assert_eq!(0, callback_invoke_count_first.load(Ordering::SeqCst));
  assert!(callback_invoke_count_second.load(Ordering::SeqCst) > 0);
}

#[tokio::test]
async fn test_pump_message_loop() {
  let mut runtime = JsRuntime::new(RuntimeOptions::default());
  poll_fn(move |cx| {
    runtime
      .execute_script_static(
        "pump_message_loop.js",
        r#"
function assertEquals(a, b) {
if (a === b) return;
throw a + " does not equal " + b;
}
const sab = new SharedArrayBuffer(16);
const i32a = new Int32Array(sab);
globalThis.resolved = false;
(function() {
const result = Atomics.waitAsync(i32a, 0, 0);
result.value.then(
  (value) => { assertEquals("ok", value); globalThis.resolved = true; },
  () => { assertUnreachable();
});
})();
const notify_return_value = Atomics.notify(i32a, 0, 1);
assertEquals(1, notify_return_value);
"#,
      )
      .unwrap();

    match runtime.poll_event_loop(cx, false) {
      Poll::Ready(Ok(())) => {}
      _ => panic!(),
    };

    // noop script, will resolve promise from first script
    runtime
      .execute_script_static("pump_message_loop2.js", r#"assertEquals(1, 1);"#)
      .unwrap();

    // check that promise from `Atomics.waitAsync` has been resolved
    runtime
      .execute_script_static(
        "pump_message_loop3.js",
        r#"assertEquals(globalThis.resolved, true);"#,
      )
      .unwrap();
    Poll::Ready(())
  })
  .await;
}

#[test]
fn test_v8_platform() {
  let options = RuntimeOptions {
    v8_platform: Some(v8::new_default_platform(0, false).make_shared()),
    ..Default::default()
  };
  let mut runtime = JsRuntime::new(options);
  runtime.execute_script_static("<none>", "").unwrap();
}

#[ignore] // TODO(@littledivy): Fast API ops when snapshot is not loaded.
#[test]
fn test_is_proxy() {
  let mut runtime = JsRuntime::new(RuntimeOptions::default());
  let all_true: v8::Global<v8::Value> = runtime
    .execute_script_static(
      "is_proxy.js",
      r#"
    (function () {
      const o = { a: 1, b: 2};
      const p = new Proxy(o, {});
      return Deno.core.ops.op_is_proxy(p) && !Deno.core.ops.op_is_proxy(o) && !Deno.core.ops.op_is_proxy(42);
    })()
  "#,
    )
    .unwrap();
  let mut scope = runtime.handle_scope();
  let all_true = v8::Local::<v8::Value>::new(&mut scope, &all_true);
  assert!(all_true.is_true());
}

#[tokio::test]
async fn test_set_macrotask_callback_set_next_tick_callback() {
  #[op2(async)]
  async fn op_async_sleep() -> Result<(), Error> {
    // Future must be Poll::Pending on first call
    tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    Ok(())
  }

  deno_core::extension!(test_ext, ops = [op_async_sleep]);
  let mut runtime = JsRuntime::new(RuntimeOptions {
    extensions: vec![test_ext::init_ops()],
    ..Default::default()
  });

  runtime
    .execute_script_static(
      "macrotasks_and_nextticks.js",
      r#"

      (async function () {
        const results = [];
        Deno.core.setMacrotaskCallback(() => {
          results.push("macrotask");
          return true;
        });
        Deno.core.setNextTickCallback(() => {
          results.push("nextTick");
          Deno.core.ops.op_set_has_tick_scheduled(false);
        });
        Deno.core.ops.op_set_has_tick_scheduled(true);
        await Deno.core.opAsync('op_async_sleep');
        if (results[0] != "nextTick") {
          throw new Error(`expected nextTick, got: ${results[0]}`);
        }
        if (results[1] != "macrotask") {
          throw new Error(`expected macrotask, got: ${results[1]}`);
        }
      })();
      "#,
    )
    .unwrap();
  runtime.run_event_loop(false).await.unwrap();
}

#[test]
fn test_has_tick_scheduled() {
  use futures::task::ArcWake;

  static MACROTASK: AtomicUsize = AtomicUsize::new(0);
  static NEXT_TICK: AtomicUsize = AtomicUsize::new(0);

  #[op2(fast)]
  fn op_macrotask() -> Result<(), AnyError> {
    MACROTASK.fetch_add(1, Ordering::Relaxed);
    Ok(())
  }

  #[op2(fast)]
  fn op_next_tick() -> Result<(), AnyError> {
    NEXT_TICK.fetch_add(1, Ordering::Relaxed);
    Ok(())
  }

  deno_core::extension!(test_ext, ops = [op_macrotask, op_next_tick]);
  let mut runtime = JsRuntime::new(RuntimeOptions {
    extensions: vec![test_ext::init_ops()],
    ..Default::default()
  });

  runtime
    .execute_script_static(
      "has_tick_scheduled.js",
      r#"
        Deno.core.setMacrotaskCallback(() => {
          Deno.core.ops.op_macrotask();
          return true; // We're done.
        });
        Deno.core.setNextTickCallback(() => Deno.core.ops.op_next_tick());
        Deno.core.ops.op_set_has_tick_scheduled(true);
        "#,
    )
    .unwrap();

  struct ArcWakeImpl(Arc<AtomicUsize>);
  impl ArcWake for ArcWakeImpl {
    fn wake_by_ref(arc_self: &Arc<Self>) {
      arc_self.0.fetch_add(1, Ordering::Relaxed);
    }
  }

  let awoken_times = Arc::new(AtomicUsize::new(0));
  let waker = futures::task::waker(Arc::new(ArcWakeImpl(awoken_times.clone())));
  let cx = &mut Context::from_waker(&waker);

  assert!(matches!(runtime.poll_event_loop(cx, false), Poll::Pending));
  assert_eq!(1, MACROTASK.load(Ordering::Relaxed));
  assert_eq!(1, NEXT_TICK.load(Ordering::Relaxed));
  assert_eq!(awoken_times.swap(0, Ordering::Relaxed), 1);
  assert!(matches!(runtime.poll_event_loop(cx, false), Poll::Pending));
  assert_eq!(awoken_times.swap(0, Ordering::Relaxed), 1);
  assert!(matches!(runtime.poll_event_loop(cx, false), Poll::Pending));
  assert_eq!(awoken_times.swap(0, Ordering::Relaxed), 1);
  assert!(matches!(runtime.poll_event_loop(cx, false), Poll::Pending));
  assert_eq!(awoken_times.swap(0, Ordering::Relaxed), 1);

  runtime
    .main_realm()
    .0
    .state()
    .borrow_mut()
    .has_next_tick_scheduled = false;
  assert!(matches!(
    runtime.poll_event_loop(cx, false),
    Poll::Ready(Ok(()))
  ));
  assert_eq!(awoken_times.load(Ordering::Relaxed), 0);
  assert!(matches!(
    runtime.poll_event_loop(cx, false),
    Poll::Ready(Ok(()))
  ));
  assert_eq!(awoken_times.load(Ordering::Relaxed), 0);
}

#[test]
fn terminate_during_module_eval() {
  let mut runtime = JsRuntime::new(RuntimeOptions {
    module_loader: Some(Rc::new(NoopModuleLoader)),
    ..Default::default()
  });

  let specifier = crate::resolve_url("file:///main.js").unwrap();
  let source_code = ascii_str!("Deno.core.print('hello\\n')");

  let module_id = futures::executor::block_on(
    runtime.load_main_module(&specifier, Some(source_code)),
  )
  .unwrap();

  runtime.v8_isolate().terminate_execution();

  let mod_result =
    futures::executor::block_on(runtime.mod_evaluate(module_id)).unwrap_err();
  assert!(mod_result
    .to_string()
    .contains("JavaScript execution has been terminated"));
}

#[tokio::test]
async fn test_unhandled_rejection_order() {
  let mut runtime = JsRuntime::new(Default::default());
  runtime
    .execute_script_static(
      "",
      r#"
      for (let i = 0; i < 100; i++) {
        Promise.reject(i);
      }
      "#,
    )
    .unwrap();
  let err = runtime.run_event_loop(false).await.unwrap_err();
  assert_eq!(err.to_string(), "Uncaught (in promise) 0");
}

#[tokio::test]
async fn test_set_promise_reject_callback() {
  static PROMISE_REJECT: AtomicUsize = AtomicUsize::new(0);

  #[op2(fast)]
  fn op_promise_reject() -> Result<(), AnyError> {
    PROMISE_REJECT.fetch_add(1, Ordering::Relaxed);
    Ok(())
  }

  deno_core::extension!(test_ext, ops = [op_promise_reject]);
  let mut runtime = JsRuntime::new(RuntimeOptions {
    extensions: vec![test_ext::init_ops()],
    ..Default::default()
  });

  runtime
    .execute_script_static(
      "promise_reject_callback.js",
      r#"
      // Note: |promise| is not the promise created below, it's a child.
      Deno.core.ops.op_set_promise_reject_callback((type, promise, reason) => {
        if (type !== /* PromiseRejectWithNoHandler */ 0) {
          throw Error("unexpected type: " + type);
        }
        if (reason.message !== "reject") {
          throw Error("unexpected reason: " + reason);
        }
        Deno.core.ops.op_store_pending_promise_rejection(promise);
        Deno.core.ops.op_promise_reject();
      });
      new Promise((_, reject) => reject(Error("reject")));
      "#,
    )
    .unwrap();
  runtime.run_event_loop(false).await.unwrap_err();

  assert_eq!(1, PROMISE_REJECT.load(Ordering::Relaxed));

  runtime
    .execute_script_static(
      "promise_reject_callback.js",
      r#"
      {
        const prev = Deno.core.ops.op_set_promise_reject_callback((...args) => {
          prev(...args);
        });
      }
      new Promise((_, reject) => reject(Error("reject")));
      "#,
    )
    .unwrap();
  runtime.run_event_loop(false).await.unwrap_err();

  assert_eq!(2, PROMISE_REJECT.load(Ordering::Relaxed));
}

#[tokio::test]
async fn test_set_promise_reject_callback_top_level_await() {
  static PROMISE_REJECT: AtomicUsize = AtomicUsize::new(0);

  #[op2(fast)]
  fn op_promise_reject(promise_type: u32) -> Result<(), AnyError> {
    eprintln!("{promise_type}");
    PROMISE_REJECT.fetch_add(1, Ordering::Relaxed);
    Ok(())
  }

  deno_core::extension!(test_ext, ops = [op_promise_reject]);

  let loader = StaticModuleLoader::with(
    Url::parse("file:///main.js").unwrap(),
    ascii_str!(
      r#"
  Deno.core.ops.op_set_promise_reject_callback((type, promise, reason) => {
    Deno.core.ops.op_promise_reject(type);
  });
  throw new Error('top level throw');
  "#
    ),
  );

  let mut runtime = JsRuntime::new(RuntimeOptions {
    extensions: vec![test_ext::init_ops()],
    module_loader: Some(Rc::new(loader)),
    ..Default::default()
  });

  let id = runtime
    .load_main_module(&crate::resolve_url("file:///main.js").unwrap(), None)
    .await
    .unwrap();
  let receiver = runtime.mod_evaluate(id);
  runtime.run_event_loop(false).await.unwrap();
  let err = receiver.await.unwrap_err();
  assert_eq!(
    err.to_string(),
    "Error: top level throw\n    at file:///main.js:5:9"
  );

  assert_eq!(1, PROMISE_REJECT.load(Ordering::Relaxed));
}

#[test]
fn test_array_by_copy() {
  // Verify that "array by copy" proposal is enabled (https://github.com/tc39/proposal-change-array-by-copy)
  let mut runtime = JsRuntime::new(Default::default());
  assert!(runtime
    .execute_script_static(
      "test_array_by_copy.js",
      "const a = [1, 2, 3];
      const b = a.toReversed();
      if (!(a[0] === 1 && a[1] === 2 && a[2] === 3)) {
        throw new Error('Expected a to be intact');
      }
      if (!(b[0] === 3 && b[1] === 2 && b[2] === 1)) {
        throw new Error('Expected b to be reversed');
      }",
    )
    .is_ok());
}

#[test]
fn test_array_from_async() {
  // Verify that "Array.fromAsync" proposal is enabled (https://github.com/tc39/proposal-array-from-async)
  let mut runtime = JsRuntime::new(Default::default());
  assert!(runtime
    .execute_script_static(
      "test_array_from_async.js",
      "(async () => {
        const b = await Array.fromAsync(new Map([[1, 2], [3, 4]]));
        if (b[0][0] !== 1 || b[0][1] !== 2 || b[1][0] !== 3 || b[1][1] !== 4) {
          throw new Error('failed');
        }
      })();",
    )
    .is_ok());
}

#[test]
fn test_iterator_helpers() {
  // Verify that "Iterator helpers" proposal is enabled (https://github.com/tc39/proposal-iterator-helpers)
  let mut runtime = JsRuntime::new(Default::default());
  assert!(runtime
    .execute_script_static(
      "test_iterator_helpers.js",
      "function* naturals() {
        let i = 0;
        while (true) {
          yield i;
          i += 1;
        }
      }
      
      const a = naturals().take(5).toArray();
      if (a[0] !== 0 || a[1] !== 1 || a[2] !== 2 || a[3] !== 3 || a[4] !== 4) {
        throw new Error('failed');
      }
      ",
    )
    .is_ok());
}

// Make sure that stalled top-level awaits (that is, top-level awaits that
// aren't tied to the progress of some op) are correctly reported, even in a
// realm other than the main one.
#[tokio::test]
async fn test_stalled_tla() {
  let loader = StaticModuleLoader::with(
    Url::parse("file:///test.js").unwrap(),
    ascii_str!("await new Promise(() => {});"),
  );
  let mut runtime = JsRuntime::new(RuntimeOptions {
    module_loader: Some(Rc::new(loader)),
    ..Default::default()
  });
  let module_id = runtime
    .load_main_module(&crate::resolve_url("file:///test.js").unwrap(), None)
    .await
    .unwrap();
  #[allow(clippy::let_underscore_future)]
  let _ = runtime.mod_evaluate(module_id);

  let error = runtime.run_event_loop(false).await.unwrap_err();
  let js_error = error.downcast::<JsError>().unwrap();
  assert_eq!(
    &js_error.exception_message,
    "Top-level await promise never resolved"
  );
  assert_eq!(js_error.frames.len(), 1);
  assert_eq!(
    js_error.frames[0].file_name.as_deref(),
    Some("file:///test.js")
  );
  assert_eq!(js_error.frames[0].line_number, Some(1));
  assert_eq!(js_error.frames[0].column_number, Some(1));
}

// Regression test for https://github.com/denoland/deno/issues/20034.
#[tokio::test]
async fn test_dynamic_import_module_error_stack() {
  #[op2(async)]
  async fn op_async_error() -> Result<(), Error> {
    tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    Err(crate::error::type_error("foo"))
  }
  deno_core::extension!(test_ext, ops = [op_async_error]);
  let loader = StaticModuleLoader::new([
    (
      Url::parse("file:///main.js").unwrap(),
      ascii_str!(
        "
        await import(\"file:///import.js\");
        "
      ),
    ),
    (
      Url::parse("file:///import.js").unwrap(),
      ascii_str!("await Deno.core.opAsync(\"op_async_error\");"),
    ),
  ]);
  let mut runtime = JsRuntime::new(RuntimeOptions {
    extensions: vec![test_ext::init_ops()],
    module_loader: Some(Rc::new(loader)),
    ..Default::default()
  });

  let module_id = runtime
    .load_main_module(&crate::resolve_url("file:///main.js").unwrap(), None)
    .await
    .unwrap();
  #[allow(clippy::let_underscore_future)]
  let _ = runtime.mod_evaluate(module_id);

  let error = runtime.run_event_loop(false).await.unwrap_err();
  let js_error = error.downcast::<JsError>().unwrap();
  assert_eq!(
    js_error.to_string(),
    "Error: foo
    at async file:///import.js:1:1"
  );
}

#[tokio::test]
#[should_panic(
  expected = "Top-level await is not allowed in extensions (mod:tla:2:1)"
)]
async fn tla_in_esm_extensions_panics() {
  #[op2(async)]
  async fn op_wait(#[number] ms: usize) {
    tokio::time::sleep(Duration::from_millis(ms as u64)).await
  }

  let extension = Extension {
    name: "test_ext",
    ops: Cow::Borrowed(&[op_wait::DECL]),
    esm_files: Cow::Borrowed(&[
      ExtensionFileSource {
        specifier: "mod:tla",
        code: ExtensionFileSourceCode::IncludedInBinary(
          r#"
await Deno.core.opAsync('op_wait', 0);
export const TEST = "foo";
        "#,
        ),
      },
      ExtensionFileSource {
        specifier: "mod:test",
        code: ExtensionFileSourceCode::IncludedInBinary("import 'mod:tla';"),
      },
    ]),
    esm_entry_point: Some("mod:test"),
    ..Default::default()
  };

  // Panics
  let _runtime = JsRuntime::new(RuntimeOptions {
    module_loader: Some(Rc::new(StaticModuleLoader::new([]))),
    extensions: vec![extension],
    ..Default::default()
  });
}
