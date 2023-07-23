// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use crate as deno_core;
use crate::modules::ModuleCode;
use crate::*;
use anyhow::Error;
use deno_ops::op;
use futures::channel::oneshot;
use futures::future::poll_fn;
use futures::future::Future;
use futures::FutureExt;
use std::cell::RefCell;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::task::Context;
use std::task::Poll;

#[tokio::test]
async fn test_set_promise_reject_callback_realms() {
  let mut runtime = JsRuntime::new(RuntimeOptions::default());
  let main_realm = runtime.main_realm();
  let realm1 = runtime.create_realm(Default::default()).unwrap();
  let realm2 = runtime.create_realm(Default::default()).unwrap();

  let realm_expectations = &[
    (&main_realm, "main_realm", 42),
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
  let main_context = runtime.main_context();
  let main_global = {
    let scope = &mut runtime.handle_scope();
    let local_global = main_context.open(scope).global(scope);
    v8::Global::new(scope, local_global)
  };

  let realm = runtime.create_realm(Default::default()).unwrap();
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
  let realm = runtime.create_realm(Default::default()).unwrap();
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
  let realm = runtime.create_realm(Default::default()).unwrap();
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
  let new_realm = runtime.create_realm(Default::default()).unwrap();

  // Test in both realms
  for realm in [runtime.main_realm(), new_realm].into_iter() {
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

  let main_realm = runtime.main_realm();
  let new_realm = runtime.create_realm(Default::default()).unwrap();

  let mut rets = vec![];

  // Test in both realms
  for realm in [main_realm, new_realm].into_iter() {
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

  let other_realm = runtime.create_realm(Default::default()).unwrap();
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
    let main_realm = runtime.main_realm();
    let other_realm = runtime.create_realm(Default::default()).unwrap();

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

#[tokio::test]
async fn test_realm_modules() {
  use std::cell::Cell;

  struct Loader(Cell<usize>);
  impl ModuleLoader for Loader {
    fn resolve(
      &self,
      specifier: &str,
      referrer: &str,
      _kind: ResolutionKind,
    ) -> Result<ModuleSpecifier, Error> {
      assert_eq!(specifier, "file:///test.js");
      assert_eq!(referrer, ".");
      let s = crate::resolve_import(specifier, referrer).unwrap();
      Ok(s)
    }

    fn load(
      &self,
      module_specifier: &ModuleSpecifier,
      _maybe_referrer: Option<&ModuleSpecifier>,
      _is_dyn_import: bool,
    ) -> Pin<Box<ModuleSourceFuture>> {
      let code = format!("export default {};", self.0.get());
      self.0.set(self.0.get() + 1);
      let module_url = module_specifier.clone();
      async move {
        Ok(ModuleSource::new(
          ModuleType::JavaScript,
          code.into(),
          &module_url,
        ))
      }
      .boxed_local()
    }
  }

  let loader = Rc::new(Loader(Cell::new(0)));
  let mut runtime = JsRuntime::new(RuntimeOptions {
    module_loader: Some(loader.clone()),
    ..Default::default()
  });
  let main_realm = runtime.main_realm();
  let other_realm = runtime
    .create_realm(CreateRealmOptions {
      module_loader: Some(loader.clone()),
    })
    .unwrap();
  let other_realm2 = runtime
    .create_realm(CreateRealmOptions {
      module_loader: Some(loader),
    })
    .unwrap();

  async fn load_test_module(runtime: &mut JsRuntime, realm: JsRealm) -> usize {
    let id = realm
      .load_side_module(
        runtime.v8_isolate(),
        &crate::resolve_url("file:///test.js").unwrap(),
        None,
      )
      .await
      .unwrap();
    let receiver = realm.mod_evaluate(runtime.v8_isolate(), id);
    runtime.run_event_loop(false).await.unwrap();
    receiver.await.unwrap().unwrap();

    let namespace = realm
      .get_module_namespace(runtime.v8_isolate(), id)
      .unwrap();
    let mut scope = realm.handle_scope(runtime.v8_isolate());
    let default_key = v8::String::new(&mut scope, "default").unwrap();
    let default_value = namespace
      .open(&mut scope)
      .get(&mut scope, default_key.into())
      .unwrap();
    assert!(default_value.is_uint32());
    default_value.uint32_value(&mut scope).unwrap() as usize
  }

  assert_eq!(load_test_module(&mut runtime, other_realm2).await, 0);
  assert_eq!(load_test_module(&mut runtime, main_realm).await, 1);
  assert_eq!(load_test_module(&mut runtime, other_realm).await, 2);
}

#[tokio::test]
async fn test_cross_realm_imports() {
  struct Loader;
  impl ModuleLoader for Loader {
    fn resolve(
      &self,
      specifier: &str,
      referrer: &str,
      _kind: ResolutionKind,
    ) -> Result<ModuleSpecifier, Error> {
      assert_eq!(specifier, "file:///test.js");
      assert_eq!(referrer, "");
      let s = crate::resolve_import(specifier, referrer).unwrap();
      Ok(s)
    }

    fn load(
      &self,
      _module_specifier: &ModuleSpecifier,
      _maybe_referrer: Option<&ModuleSpecifier>,
      _is_dyn_import: bool,
    ) -> Pin<Box<ModuleSourceFuture>> {
      async {
        Ok(ModuleSource::for_test(
          "export default globalThis;",
          "file:///test.js",
        ))
      }
      .boxed_local()
    }
  }

  let loader = Rc::new(Loader);
  let mut runtime = JsRuntime::new(RuntimeOptions {
    module_loader: Some(loader.clone()),
    ..Default::default()
  });
  let main_realm = runtime.main_realm();
  let other_realm = runtime
    .create_realm(CreateRealmOptions {
      module_loader: Some(loader),
    })
    .unwrap();

  fn import_wrapper_function(
    realm: &JsRealm,
    isolate: &mut v8::Isolate,
  ) -> v8::Global<v8::Function> {
    let value = realm
      .execute_script_static(
        isolate,
        "",
        r#"() => import("file:///test.js").then(ns => ns.default)"#,
      )
      .unwrap();

    let mut scope = realm.handle_scope(isolate);
    let value = v8::Local::new(&mut scope, value);
    let function: v8::Local<v8::Function> = value.try_into().unwrap();
    v8::Global::new(&mut scope, function)
  }

  let main_import_wrapper =
    import_wrapper_function(&main_realm, runtime.v8_isolate());
  let other_import_wrapper =
    import_wrapper_function(&other_realm, runtime.v8_isolate());

  async fn run_fn_test(
    runtime: &mut JsRuntime,
    realm: &JsRealm,
    function: v8::Global<v8::Function>,
  ) -> v8::Global<v8::Value> {
    let promise = {
      let mut scope = realm.handle_scope(runtime.v8_isolate());
      let undefined = v8::undefined(&mut scope);
      let promise = function
        .open(&mut scope)
        .call(&mut scope, undefined.into(), &[])
        .unwrap();
      assert!(promise.is_promise());
      v8::Global::new(&mut scope, promise)
    };
    runtime.resolve_value(promise).await.unwrap()
  }

  // Same-realm imports.
  assert_eq!(
    run_fn_test(&mut runtime, &main_realm, main_import_wrapper.clone()).await,
    main_realm.global_object(runtime.v8_isolate())
  );
  assert_eq!(
    run_fn_test(&mut runtime, &other_realm, other_import_wrapper.clone()).await,
    other_realm.global_object(runtime.v8_isolate())
  );

  // Cross-realm imports.
  assert_eq!(
    run_fn_test(&mut runtime, &main_realm, other_import_wrapper.clone()).await,
    other_realm.global_object(runtime.v8_isolate())
  );
  assert_eq!(
    run_fn_test(&mut runtime, &other_realm, main_import_wrapper.clone()).await,
    main_realm.global_object(runtime.v8_isolate())
  );
}

// Make sure that loading and evaluating top-level-imported modules in
// different realms at the same time works.
#[tokio::test]
async fn test_realms_concurrent_module_evaluations() {
  struct Loader;
  impl ModuleLoader for Loader {
    fn resolve(
      &self,
      specifier: &str,
      referrer: &str,
      _kind: ResolutionKind,
    ) -> Result<ModuleSpecifier, Error> {
      assert_eq!(specifier, "file:///test.js");
      assert_eq!(referrer, ".");
      let s = crate::resolve_import(specifier, referrer).unwrap();
      Ok(s)
    }

    fn load(
      &self,
      _module_specifier: &ModuleSpecifier,
      _maybe_referrer: Option<&ModuleSpecifier>,
      _is_dyn_import: bool,
    ) -> Pin<Box<ModuleSourceFuture>> {
      async {
        Ok(ModuleSource::for_test(
          r#"await Deno.core.opAsync("op_wait");"#,
          "file:///test.js",
        ))
      }
      .boxed_local()
    }
  }

  #[op]
  async fn op_wait(op_state: Rc<RefCell<OpState>>) {
    let (sender, receiver) = oneshot::channel::<()>();
    op_state
      .borrow_mut()
      .borrow_mut::<Vec<oneshot::Sender<()>>>()
      .push(sender);
    receiver.await.unwrap();
  }
  deno_core::extension!(test_ext, ops = [op_wait]);

  let mut runtime = JsRuntime::new(RuntimeOptions {
    extensions: vec![test_ext::init_ops()],
    module_loader: Some(Rc::new(Loader)),
    ..Default::default()
  });
  runtime
    .op_state()
    .borrow_mut()
    .put::<Vec<oneshot::Sender<()>>>(vec![]);

  let main_realm = runtime.main_realm();
  let other_realm = runtime
    .create_realm(CreateRealmOptions {
      module_loader: Some(Rc::new(Loader)),
    })
    .unwrap();

  let main_realm_id = main_realm
    .load_main_module(
      runtime.v8_isolate(),
      &crate::resolve_url("file:///test.js").unwrap(),
      None,
    )
    .await
    .unwrap();
  let main_realm_receiver =
    main_realm.mod_evaluate(runtime.v8_isolate(), main_realm_id);

  let other_realm_id = other_realm
    .load_main_module(
      runtime.v8_isolate(),
      &crate::resolve_url("file:///test.js").unwrap(),
      None,
    )
    .await
    .unwrap();
  let other_realm_receiver =
    other_realm.mod_evaluate(runtime.v8_isolate(), other_realm_id);

  poll_fn(|cx| {
    let res = runtime.poll_event_loop(cx, false);
    assert!(matches!(res, Poll::Pending));
    Poll::Ready(())
  })
  .await;

  // Resolve the promises
  {
    let senders = runtime
      .op_state()
      .borrow_mut()
      .take::<Vec<oneshot::Sender<()>>>();
    for sender in senders {
      sender.send(()).unwrap();
    }
  }

  runtime.run_event_loop(false).await.unwrap();
  assert!(matches!(main_realm_receiver.await, Ok(Ok(()))));
  assert!(matches!(other_realm_receiver.await, Ok(Ok(()))));
}

// Make sure that loading and evaluating dynamic imported modules in different
// realms at the same time works.
#[tokio::test]
async fn test_realm_concurrent_dynamic_imports() {
  struct Loader;
  impl ModuleLoader for Loader {
    fn resolve(
      &self,
      specifier: &str,
      referrer: &str,
      _kind: ResolutionKind,
    ) -> Result<ModuleSpecifier, Error> {
      assert_eq!(specifier, "file:///test.js");
      assert_eq!(referrer, "");
      let s = crate::resolve_import(specifier, referrer).unwrap();
      Ok(s)
    }

    fn load(
      &self,
      _module_specifier: &ModuleSpecifier,
      _maybe_referrer: Option<&ModuleSpecifier>,
      _is_dyn_import: bool,
    ) -> Pin<Box<ModuleSourceFuture>> {
      async {
        Ok(ModuleSource::for_test(
          r#"await Deno.core.opAsync("op_wait");"#,
          "file:///test.js",
        ))
      }
      .boxed_local()
    }
  }

  #[op]
  async fn op_wait(op_state: Rc<RefCell<OpState>>) {
    let (sender, receiver) = oneshot::channel::<()>();
    op_state
      .borrow_mut()
      .borrow_mut::<Vec<oneshot::Sender<()>>>()
      .push(sender);
    receiver.await.unwrap();
  }
  deno_core::extension!(test_ext, ops = [op_wait]);

  let mut runtime = JsRuntime::new(RuntimeOptions {
    extensions: vec![test_ext::init_ops()],
    module_loader: Some(Rc::new(Loader)),
    ..Default::default()
  });
  runtime
    .op_state()
    .borrow_mut()
    .put::<Vec<oneshot::Sender<()>>>(vec![]);

  let main_realm = runtime.main_realm();
  let other_realm = runtime
    .create_realm(CreateRealmOptions {
      module_loader: Some(Rc::new(Loader)),
    })
    .unwrap();

  let main_realm_promise = {
    let global = main_realm
      .execute_script_static(
        runtime.v8_isolate(),
        "",
        r#"import("file:///test.js")"#,
      )
      .unwrap();
    let scope = &mut main_realm.handle_scope(runtime.v8_isolate());
    let local = v8::Local::new(scope, global);
    let promise = v8::Local::<v8::Promise>::try_from(local).unwrap();
    assert_eq!(promise.state(), v8::PromiseState::Pending);
    v8::Global::new(scope, promise)
  };
  let other_realm_promise = {
    let global = other_realm
      .execute_script_static(
        runtime.v8_isolate(),
        "",
        r#"import("file:///test.js")"#,
      )
      .unwrap();
    let scope = &mut other_realm.handle_scope(runtime.v8_isolate());
    let local = v8::Local::new(scope, global);
    let promise = v8::Local::<v8::Promise>::try_from(local).unwrap();
    assert_eq!(promise.state(), v8::PromiseState::Pending);
    v8::Global::new(scope, promise)
  };

  poll_fn(|cx| {
    let res = runtime.poll_event_loop(cx, false);
    assert!(matches!(res, Poll::Pending));
    Poll::Ready(())
  })
  .await;

  // Resolve the promises
  {
    let senders = runtime
      .op_state()
      .borrow_mut()
      .take::<Vec<oneshot::Sender<()>>>();
    for sender in senders {
      sender.send(()).unwrap();
    }
  }

  runtime.run_event_loop(false).await.unwrap();
  assert_eq!(
    main_realm_promise.open(runtime.v8_isolate()).state(),
    v8::PromiseState::Fulfilled
  );
  assert_eq!(
    other_realm_promise.open(runtime.v8_isolate()).state(),
    v8::PromiseState::Fulfilled
  );
}

#[test]
fn es_snapshot() {
  let startup_data = {
    let extension = Extension::builder("module_snapshot")
      .esm(vec![ExtensionFileSource {
        specifier: "mod:test",
        code: ExtensionFileSourceCode::IncludedInBinary(
          "globalThis.TEST = 'foo'; export const TEST = 'bar';",
        ),
      }])
      .esm_entry_point("mod:test")
      .build();

    let runtime = JsRuntimeForSnapshot::new(
      RuntimeOptions {
        extensions: vec![extension],
        ..Default::default()
      },
      Default::default(),
    );
    runtime.snapshot()
  };

  let mut runtime = JsRuntime::new(RuntimeOptions {
    module_loader: None,
    startup_snapshot: Some(Snapshot::JustCreated(startup_data)),
    ..Default::default()
  });

  struct Loader;

  impl ModuleLoader for Loader {
    fn resolve(
      &self,
      specifier: &str,
      referrer: &str,
      _kind: ResolutionKind,
    ) -> Result<ModuleSpecifier, Error> {
      assert_eq!(specifier, "mod:test");
      Ok(resolve_import(specifier, referrer)?)
    }

    fn load(
      &self,
      _module_specifier: &ModuleSpecifier,
      _maybe_referrer: Option<&ModuleSpecifier>,
      _is_dyn_import: bool,
    ) -> Pin<Box<ModuleSourceFuture>> {
      unreachable!()
    }
  }

  let realm = runtime
    .create_realm(CreateRealmOptions {
      module_loader: Some(Rc::new(Loader)),
    })
    .unwrap();

  // The module was evaluated ahead of time
  {
    let global_test = realm
      .execute_script_static(runtime.v8_isolate(), "", "globalThis.TEST")
      .unwrap();
    let scope = &mut realm.handle_scope(runtime.v8_isolate());
    let global_test = v8::Local::new(scope, global_test);
    assert!(global_test.is_string());
    assert_eq!(global_test.to_rust_string_lossy(scope).as_str(), "foo");
  }

  // The module can be imported
  {
    let test_export_promise = realm
      .execute_script_static(
        runtime.v8_isolate(),
        "",
        "import('mod:test').then(module => module.TEST)",
      )
      .unwrap();
    let test_export =
      futures::executor::block_on(runtime.resolve_value(test_export_promise))
        .unwrap();

    let scope = &mut realm.handle_scope(runtime.v8_isolate());
    let test_export = v8::Local::new(scope, test_export);
    assert!(test_export.is_string());
    assert_eq!(test_export.to_rust_string_lossy(scope).as_str(), "bar");
  }
}

#[test]
fn es_snapshot_without_runtime_module_loader_in_realm() {
  let startup_data = {
    let extension = Extension::builder("module_snapshot")
      .esm(vec![ExtensionFileSource {
        specifier: "ext:module_snapshot/test.js",
        code: ExtensionFileSourceCode::IncludedInBinary(
          "globalThis.TEST = 'foo'; export const TEST = 'bar';",
        ),
      }])
      .esm_entry_point("ext:module_snapshot/test.js")
      .build();

    let runtime = JsRuntimeForSnapshot::new(
      RuntimeOptions {
        extensions: vec![extension],
        ..Default::default()
      },
      Default::default(),
    );

    runtime.snapshot()
  };

  struct MainRealmLoader;

  impl ModuleLoader for MainRealmLoader {
    fn resolve(
      &self,
      _specifier: &str,
      _referrer: &str,
      _kind: ResolutionKind,
    ) -> Result<ModuleSpecifier, Error> {
      unreachable!()
    }

    fn load(
      &self,
      _module_specifier: &ModuleSpecifier,
      _maybe_referrer: Option<&ModuleSpecifier>,
      _is_dyn_import: bool,
    ) -> Pin<Box<ModuleSourceFuture>> {
      unreachable!()
    }
  }

  let mut runtime = JsRuntime::new(RuntimeOptions {
    module_loader: Some(Rc::new(MainRealmLoader)),
    startup_snapshot: Some(Snapshot::JustCreated(startup_data)),
    ..Default::default()
  });

  let realm = runtime
    .create_realm(CreateRealmOptions {
      module_loader: None,
    })
    .unwrap();

  // Make sure the module was evaluated.
  {
    let scope = &mut realm.handle_scope(runtime.v8_isolate());
    let global_test: v8::Local<v8::String> =
      JsRuntime::eval(scope, "globalThis.TEST").unwrap();
    assert_eq!(
      serde_v8::to_utf8(global_test.to_string(scope).unwrap(), scope),
      String::from("foo"),
    );
  }

  // Make sure we can't import the module.
  let dyn_import_promise = realm
    .execute_script_static(
      runtime.v8_isolate(),
      "",
      "import('ext:module_snapshot/test.js')",
    )
    .unwrap();
  let dyn_import_result =
    futures::executor::block_on(runtime.resolve_value(dyn_import_promise));
  assert!(dyn_import_result.is_err());
  assert_eq!(
    dyn_import_result.err().unwrap().to_string().as_str(),
    r#"Uncaught TypeError: Module loading is not supported; attempted to resolve: "ext:module_snapshot/test.js" from """#
  );
}

#[test]
fn preserve_snapshotted_modules() {
  let startup_data = {
    let extension = Extension::builder("module_snapshot")
      .esm(vec![
        ExtensionFileSource {
          specifier: "test:preserved",
          code: ExtensionFileSourceCode::IncludedInBinary(
            "export const TEST = 'foo';",
          ),
        },
        ExtensionFileSource {
          specifier: "test:not-preserved",
          code: ExtensionFileSourceCode::IncludedInBinary(
            "import 'test:preserved'; export const TEST = 'bar';",
          ),
        },
      ])
      .esm_entry_point("test:not-preserved")
      .build();

    let runtime = JsRuntimeForSnapshot::new(
      RuntimeOptions {
        extensions: vec![extension],
        ..Default::default()
      },
      Default::default(),
    );

    runtime.snapshot()
  };

  struct Loader;

  impl ModuleLoader for Loader {
    fn resolve(
      &self,
      specifier: &str,
      referrer: &str,
      _kind: ResolutionKind,
    ) -> Result<ModuleSpecifier, Error> {
      Ok(resolve_import(specifier, referrer)?)
    }

    fn load(
      &self,
      module_specifier: &ModuleSpecifier,
      _maybe_referrer: Option<&ModuleSpecifier>,
      _is_dyn_import: bool,
    ) -> Pin<Box<ModuleSourceFuture>> {
      assert_eq!(module_specifier.as_str(), "test:not-preserved");
      futures::future::ready(Err(error::generic_error("Couldn't load module")))
        .boxed_local()
    }
  }

  let loader = Rc::new(Loader);

  let mut runtime = JsRuntime::new(RuntimeOptions {
    module_loader: Some(loader.clone()),
    startup_snapshot: Some(Snapshot::JustCreated(startup_data)),
    preserve_snapshotted_modules: Some(&["test:preserved"]),
    ..Default::default()
  });

  let realm = runtime
    .create_realm(CreateRealmOptions {
      module_loader: Some(loader),
    })
    .unwrap();

  // We can't import "test:not-preserved"
  {
    let dyn_import_promise = realm
      .execute_script_static(
        runtime.v8_isolate(),
        "",
        "import('test:not-preserved')",
      )
      .unwrap();
    let dyn_import_result =
      futures::executor::block_on(runtime.resolve_value(dyn_import_promise));
    assert!(dyn_import_result.is_err());
    assert_eq!(
      dyn_import_result.err().unwrap().to_string().as_str(),
      "Uncaught TypeError: Couldn't load module"
    );
  }

  // But we can import "test:preserved"
  {
    let dyn_import_promise = realm
      .execute_script_static(
        runtime.v8_isolate(),
        "",
        "import('test:preserved').then(module => module.TEST)",
      )
      .unwrap();
    let dyn_import_result =
      futures::executor::block_on(runtime.resolve_value(dyn_import_promise))
        .unwrap();
    let scope = &mut realm.handle_scope(runtime.v8_isolate());
    assert!(dyn_import_result.open(scope).is_string());
    assert_eq!(
      dyn_import_result
        .open(scope)
        .to_rust_string_lossy(scope)
        .as_str(),
      "foo"
    );
  }
}

/// Test that `RuntimeOptions::preserve_snapshotted_modules` also works without
/// a snapshot.
#[test]
fn non_snapshot_preserve_snapshotted_modules() {
  let extension = Extension::builder("esm_extension")
    .esm(vec![
      ExtensionFileSource {
        specifier: "test:preserved",
        code: ExtensionFileSourceCode::IncludedInBinary(
          "export const TEST = 'foo';",
        ),
      },
      ExtensionFileSource {
        specifier: "test:not-preserved",
        code: ExtensionFileSourceCode::IncludedInBinary(
          "import 'test:preserved'; export const TEST = 'bar';",
        ),
      },
    ])
    .esm_entry_point("test:not-preserved")
    .build();

  let mut runtime = JsRuntime::new(RuntimeOptions {
    module_loader: None,
    extensions: vec![extension],
    preserve_snapshotted_modules: Some(&["test:preserved"]),
    ..Default::default()
  });

  struct Loader;

  impl ModuleLoader for Loader {
    fn resolve(
      &self,
      specifier: &str,
      referrer: &str,
      _kind: ResolutionKind,
    ) -> Result<ModuleSpecifier, Error> {
      Ok(resolve_import(specifier, referrer)?)
    }

    fn load(
      &self,
      module_specifier: &ModuleSpecifier,
      _maybe_referrer: Option<&ModuleSpecifier>,
      _is_dyn_import: bool,
    ) -> Pin<Box<ModuleSourceFuture>> {
      assert_eq!(module_specifier.as_str(), "test:not-preserved");
      futures::future::ready(Err(error::generic_error("Couldn't load module")))
        .boxed_local()
    }
  }

  let realm = runtime
    .create_realm(CreateRealmOptions {
      module_loader: Some(Rc::new(Loader)),
    })
    .unwrap();

  // We can't import "test:not-preserved"
  {
    let dyn_import_promise = realm
      .execute_script_static(
        runtime.v8_isolate(),
        "",
        "import('test:not-preserved')",
      )
      .unwrap();
    let dyn_import_result =
      futures::executor::block_on(runtime.resolve_value(dyn_import_promise));
    assert!(dyn_import_result.is_err());
    assert_eq!(
      dyn_import_result.err().unwrap().to_string().as_str(),
      "Uncaught TypeError: Couldn't load module"
    );
  }

  // But we can import "test:preserved"
  {
    let dyn_import_promise = realm
      .execute_script_static(
        runtime.v8_isolate(),
        "",
        "import('test:preserved').then(module => module.TEST)",
      )
      .unwrap();
    let dyn_import_result =
      futures::executor::block_on(runtime.resolve_value(dyn_import_promise))
        .unwrap();
    let scope = &mut realm.handle_scope(runtime.v8_isolate());
    assert!(dyn_import_result.open(scope).is_string());
    assert_eq!(
      dyn_import_result
        .open(scope)
        .to_rust_string_lossy(scope)
        .as_str(),
      "foo"
    );
  }
}
