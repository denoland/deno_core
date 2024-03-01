// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
use crate::error;
use crate::modules::StaticModuleLoader;
use crate::op2;
use crate::JsRuntime;
use crate::JsRuntimeForSnapshot;
use crate::RuntimeOptions;
use futures::future::poll_fn;
use std::rc::Rc;
use std::task::Poll;

#[test]
fn test_set_format_exception_callback_realms() {
  let mut runtime = JsRuntime::new(RuntimeOptions::default());
  let main_realm = runtime.main_realm();

  let realm_expectations = &[(&main_realm, "main_realm")];

  // Set up format exception callbacks.
  for (realm, realm_name) in realm_expectations {
    realm
      .execute_script(
        runtime.v8_isolate(),
        "",
        format!(
          r#"
          Deno.core.ops.op_set_format_exception_callback((error) => {{
            return `{realm_name} / ${{error}}`;
          }});
        "#
        ),
      )
      .unwrap();
  }

  for (realm, realm_name) in realm_expectations {
    // Immediate exceptions
    {
      let result = realm.execute_script(
        runtime.v8_isolate(),
        "",
        format!("throw new Error('{realm_name}');"),
      );
      assert!(result.is_err());

      let error = result.unwrap_err().downcast::<error::JsError>().unwrap();
      assert_eq!(
        error.exception_message,
        format!("{realm_name} / Error: {realm_name}")
      );
    }

    // Promise rejections
    {
      realm
        .execute_script(
          runtime.v8_isolate(),
          "",
          format!("Promise.reject(new Error('{realm_name}'));"),
        )
        .unwrap();

      let result =
        futures::executor::block_on(runtime.run_event_loop(Default::default()));
      assert!(result.is_err());
      let error = result.unwrap_err().downcast::<error::JsError>().unwrap();
      assert_eq!(
        error.exception_message,
        format!("Uncaught (in promise) {realm_name} / Error: {realm_name}")
      );
    }
  }
}

#[tokio::test]
async fn js_realm_ref_unref_ops() {
  // Never resolves.
  #[op2(async)]
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

    main_realm
      .execute_script(
        runtime.v8_isolate(),
        "",
        r#"
        const { op_pending } = Deno.core.ensureFastOps();
        var promise = op_pending();
        "#,
      )
      .unwrap();
    assert!(matches!(
      runtime.poll_event_loop(cx, Default::default()),
      Poll::Pending
    ));

    main_realm
      .execute_script(
        runtime.v8_isolate(),
        "",
        r#"
          Deno.core.unrefOpPromise(promise);
        "#,
      )
      .unwrap();

    assert!(matches!(
      runtime.poll_event_loop(cx, Default::default()),
      Poll::Ready(Ok(()))
    ));
    Poll::Ready(())
  })
  .await;
}

#[test]
fn es_snapshot() {
  let startup_data = {
    deno_core::extension!(
      module_snapshot,
      esm_entry_point = "mod:test",
      esm = ["mod:test" =
        { source = "globalThis.TEST = 'foo'; export const TEST = 'bar';" },]
    );

    let runtime = JsRuntimeForSnapshot::new(RuntimeOptions {
      extensions: vec![module_snapshot::init_ops_and_esm()],
      module_loader: Some(Rc::new(StaticModuleLoader::default())),
      ..Default::default()
    });
    runtime.snapshot()
  };
  let snapshot = Box::leak(startup_data);
  let mut runtime = JsRuntime::new(RuntimeOptions {
    module_loader: None,
    startup_snapshot: Some(snapshot),
    ..Default::default()
  });

  // The module was evaluated ahead of time
  {
    let global_test = runtime.execute_script("", "globalThis.TEST").unwrap();
    let scope = &mut runtime.handle_scope();
    let global_test = v8::Local::new(scope, global_test);
    assert!(global_test.is_string());
    assert_eq!(global_test.to_rust_string_lossy(scope).as_str(), "foo");
  }

  // The module can be imported
  {
    let test_export_promise = runtime
      .execute_script("", "import('mod:test').then(module => module.TEST)")
      .unwrap();
    #[allow(deprecated)]
    let test_export =
      futures::executor::block_on(runtime.resolve_value(test_export_promise))
        .unwrap();

    let scope = &mut runtime.handle_scope();
    let test_export = v8::Local::new(scope, test_export);
    assert!(test_export.is_string());
    assert_eq!(test_export.to_rust_string_lossy(scope).as_str(), "bar");
  }
}
