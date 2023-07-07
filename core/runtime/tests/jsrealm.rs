// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use crate as deno_core;
use crate::ascii_str;
use crate::error::custom_error;
use crate::error::generic_error;
use crate::error::AnyError;
use crate::error::JsError;
use crate::extensions::OpDecl;
use crate::include_ascii_string;
use crate::module_specifier::ModuleSpecifier;
use crate::modules::AssertedModuleType;
use crate::modules::ModuleCode;
use crate::modules::ModuleInfo;
use crate::modules::ModuleLoadId;
use crate::modules::ModuleLoader;
use crate::modules::ModuleSource;
use crate::modules::ModuleSourceFuture;
use crate::modules::ModuleType;
use crate::modules::ResolutionKind;
use crate::modules::SymbolicModule;
use crate::Extension;
use crate::JsBuffer;
use crate::*;
use anyhow::Error;
use cooked_waker::IntoWaker;
use cooked_waker::Wake;
use cooked_waker::WakeRef;
use deno_ops::op;
use futures::future::poll_fn;
use futures::future::Future;
use futures::FutureExt;
use std::cell::RefCell;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicI8;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::task::Context;
use std::task::Poll;
use std::time::Duration;

#[tokio::test]
async fn test_set_promise_reject_callback_realms() {
  let mut runtime = JsRuntime::new(RuntimeOptions::default());
  let global_realm = runtime.global_realm();
  let realm1 = runtime.create_realm().unwrap();
  let realm2 = runtime.create_realm().unwrap();

  let realm_expectations = &[
    (&global_realm, "global_realm", 42),
    (&realm1, "realm1", 140),
    (&realm2, "realm2", 720),
  ];

  // Set up promise reject callbacks.
  for (realm, realm_name, number) in realm_expectations {
    realm
      .execute_script(
        runtime.v8_isolate(),
        "",
        format!(
          r#"

            globalThis.rejectValue = undefined;
            Deno.core.setPromiseRejectCallback((_type, _promise, reason) => {{
              globalThis.rejectValue = `{realm_name}/${{reason}}`;
            }});
            Deno.core.opAsync("op_void_async").then(() => Promise.reject({number}));
          "#
        ).into()
      )
      .unwrap();
  }

  runtime.run_event_loop(false).await.unwrap();

  for (realm, realm_name, number) in realm_expectations {
    let reject_value = realm
      .execute_script_static(runtime.v8_isolate(), "", "globalThis.rejectValue")
      .unwrap();
    let scope = &mut realm.handle_scope(runtime.v8_isolate());
    let reject_value = v8::Local::new(scope, reject_value);
    assert!(reject_value.is_string());
    let reject_value_string = reject_value.to_rust_string_lossy(scope);
    assert_eq!(reject_value_string, format!("{realm_name}/{number}"));
  }
}

#[test]
fn js_realm_simple() {
  let mut runtime = JsRuntime::new(Default::default());
  let main_context = runtime.global_context();
  let main_global = {
    let scope = &mut runtime.handle_scope();
    let local_global = main_context.open(scope).global(scope);
    v8::Global::new(scope, local_global)
  };

  let realm = runtime.create_realm().unwrap();
  assert_ne!(realm.context(), &main_context);
  assert_ne!(realm.global_object(runtime.v8_isolate()), main_global);

  let main_object = runtime.execute_script_static("", "Object").unwrap();
  let realm_object = realm
    .execute_script_static(runtime.v8_isolate(), "", "Object")
    .unwrap();
  assert_ne!(main_object, realm_object);
}

#[test]
fn js_realm_init() {
  #[op]
  fn op_test() -> Result<String, Error> {
    Ok(String::from("Test"))
  }

  deno_core::extension!(test_ext, ops = [op_test]);
  let mut runtime = JsRuntime::new(RuntimeOptions {
    extensions: vec![test_ext::init_ops()],
    ..Default::default()
  });
  let realm = runtime.create_realm().unwrap();
  let ret = realm
    .execute_script_static(runtime.v8_isolate(), "", "Deno.core.ops.op_test()")
    .unwrap();

  let scope = &mut realm.handle_scope(runtime.v8_isolate());
  assert_eq!(ret, serde_v8::to_v8(scope, "Test").unwrap());
}

#[test]
fn js_realm_init_snapshot() {
  let snapshot = {
    let runtime =
      JsRuntimeForSnapshot::new(Default::default(), Default::default());
    let snap: &[u8] = &runtime.snapshot();
    Vec::from(snap).into_boxed_slice()
  };

  #[op]
  fn op_test() -> Result<String, Error> {
    Ok(String::from("Test"))
  }

  deno_core::extension!(test_ext, ops = [op_test]);
  let mut runtime = JsRuntime::new(RuntimeOptions {
    startup_snapshot: Some(Snapshot::Boxed(snapshot)),
    extensions: vec![test_ext::init_ops()],
    ..Default::default()
  });
  let realm = runtime.create_realm().unwrap();
  let ret = realm
    .execute_script_static(runtime.v8_isolate(), "", "Deno.core.ops.op_test()")
    .unwrap();

  let scope = &mut realm.handle_scope(runtime.v8_isolate());
  assert_eq!(ret, serde_v8::to_v8(scope, "Test").unwrap());
}

#[test]
fn js_realm_sync_ops() {
  // Test that returning a RustToV8Buf and throwing an exception from a sync
  // op result in objects with prototypes from the right realm. Note that we
  // don't test the result of returning structs, because they will be
  // serialized to objects with null prototype.

  #[op]
  fn op_test(fail: bool) -> Result<ToJsBuffer, Error> {
    if !fail {
      Ok(ToJsBuffer::empty())
    } else {
      Err(crate::error::type_error("Test"))
    }
  }

  deno_core::extension!(test_ext, ops = [op_test]);
  let mut runtime = JsRuntime::new(RuntimeOptions {
    extensions: vec![test_ext::init_ops()],
    get_error_class_fn: Some(&|error| {
      crate::error::get_custom_error_class(error).unwrap()
    }),
    ..Default::default()
  });
  let new_realm = runtime.create_realm().unwrap();

  // Test in both realms
  for realm in [runtime.global_realm(), new_realm].into_iter() {
    let ret = realm
      .execute_script_static(
        runtime.v8_isolate(),
        "",
        r#"
          const buf = Deno.core.ops.op_test(false);
          try {
            Deno.core.ops.op_test(true);
          } catch(e) {
            err = e;
          }
          buf instanceof Uint8Array && buf.byteLength === 0 &&
          err instanceof TypeError && err.message === "Test"
        "#,
      )
      .unwrap();
    assert!(ret.open(runtime.v8_isolate()).is_true());
  }
}

#[tokio::test]
async fn js_realm_async_ops() {
  // Test that returning a RustToV8Buf and throwing an exception from a async
  // op result in objects with prototypes from the right realm. Note that we
  // don't test the result of returning structs, because they will be
  // serialized to objects with null prototype.

  #[op]
  async fn op_test(fail: bool) -> Result<ToJsBuffer, Error> {
    if !fail {
      Ok(ToJsBuffer::empty())
    } else {
      Err(crate::error::type_error("Test"))
    }
  }

  deno_core::extension!(test_ext, ops = [op_test]);
  let mut runtime = JsRuntime::new(RuntimeOptions {
    extensions: vec![test_ext::init_ops()],
    get_error_class_fn: Some(&|error| {
      crate::error::get_custom_error_class(error).unwrap()
    }),
    ..Default::default()
  });

  let global_realm = runtime.global_realm();
  let new_realm = runtime.create_realm().unwrap();

  let mut rets = vec![];

  // Test in both realms
  for realm in [global_realm, new_realm].into_iter() {
    let ret = realm
      .execute_script_static(
        runtime.v8_isolate(),
        "",
        r#"

          (async function () {
            const buf = await Deno.core.opAsync("op_test", false);
            let err;
            try {
              await Deno.core.opAsync("op_test", true);
            } catch(e) {
              err = e;
            }
            return buf instanceof Uint8Array && buf.byteLength === 0 &&
                    err instanceof TypeError && err.message === "Test" ;
          })();
        "#,
      )
      .unwrap();
    rets.push((realm, ret));
  }

  runtime.run_event_loop(false).await.unwrap();

  for ret in rets {
    let scope = &mut ret.0.handle_scope(runtime.v8_isolate());
    let value = v8::Local::new(scope, ret.1);
    let promise = v8::Local::<v8::Promise>::try_from(value).unwrap();
    let result = promise.result(scope);

    assert!(result.is_boolean() && result.is_true());
  }
}

#[ignore]
#[tokio::test]
async fn js_realm_gc() {
  static INVOKE_COUNT: AtomicUsize = AtomicUsize::new(0);
  struct PendingFuture {}

  impl Future for PendingFuture {
    type Output = ();
    fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<()> {
      Poll::Pending
    }
  }

  impl Drop for PendingFuture {
    fn drop(&mut self) {
      assert_eq!(INVOKE_COUNT.fetch_sub(1, Ordering::SeqCst), 1);
    }
  }

  // Never resolves.
  #[op]
  async fn op_pending() {
    assert_eq!(INVOKE_COUNT.fetch_add(1, Ordering::SeqCst), 0);
    PendingFuture {}.await
  }

  deno_core::extension!(test_ext, ops = [op_pending]);
  let mut runtime = JsRuntime::new(RuntimeOptions {
    extensions: vec![test_ext::init_ops()],
    ..Default::default()
  });

  // Detect a drop in OpState
  let opstate_drop_detect = Rc::new(());
  runtime
    .op_state()
    .borrow_mut()
    .put(opstate_drop_detect.clone());
  assert_eq!(Rc::strong_count(&opstate_drop_detect), 2);

  let other_realm = runtime.create_realm().unwrap();
  other_realm
    .execute_script(
      runtime.v8_isolate(),
      "future",
      ModuleCode::from_static("Deno.core.opAsync('op_pending')"),
    )
    .unwrap();
  while INVOKE_COUNT.load(Ordering::SeqCst) == 0 {
    poll_fn(|cx: &mut Context| runtime.poll_event_loop(cx, false))
      .await
      .unwrap();
  }
  drop(other_realm);
  while INVOKE_COUNT.load(Ordering::SeqCst) == 1 {
    poll_fn(|cx| runtime.poll_event_loop(cx, false))
      .await
      .unwrap();
  }
  drop(runtime);

  // Make sure the OpState was dropped properly when the runtime dropped
  assert_eq!(Rc::strong_count(&opstate_drop_detect), 1);
}

#[tokio::test]
async fn js_realm_ref_unref_ops() {
  // Never resolves.
  #[op]
  async fn op_pending() {
    futures::future::pending().await
  }

  deno_core::extension!(test_ext, ops = [op_pending]);
  let mut runtime = JsRuntime::new(RuntimeOptions {
    extensions: vec![test_ext::init_ops()],
    ..Default::default()
  });

  poll_fn(move |cx| {
    let main_realm = runtime.global_realm();
    let other_realm = runtime.create_realm().unwrap();

    main_realm
      .execute_script_static(
        runtime.v8_isolate(),
        "",
        r#"

          var promise = Deno.core.opAsync("op_pending");
        "#,
      )
      .unwrap();
    other_realm
      .execute_script_static(
        runtime.v8_isolate(),
        "",
        r#"

          var promise = Deno.core.opAsync("op_pending");
        "#,
      )
      .unwrap();
    assert!(matches!(runtime.poll_event_loop(cx, false), Poll::Pending));

    main_realm
      .execute_script_static(
        runtime.v8_isolate(),
        "",
        r#"
          let promiseIdSymbol = Symbol.for("Deno.core.internalPromiseId");
          Deno.core.unrefOp(promise[promiseIdSymbol]);
        "#,
      )
      .unwrap();
    assert!(matches!(runtime.poll_event_loop(cx, false), Poll::Pending));

    other_realm
      .execute_script_static(
        runtime.v8_isolate(),
        "",
        r#"
          let promiseIdSymbol = Symbol.for("Deno.core.internalPromiseId");
          Deno.core.unrefOp(promise[promiseIdSymbol]);
        "#,
      )
      .unwrap();
    assert!(matches!(
      runtime.poll_event_loop(cx, false),
      Poll::Ready(Ok(()))
    ));
    Poll::Ready(())
  })
  .await;
}
