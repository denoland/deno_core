// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use crate::extensions::Op;
use crate::modules::AssertedModuleType;
use crate::modules::LoggingModuleLoader;
use crate::modules::ModuleInfo;
use crate::*;
use anyhow::Error;
use std::borrow::Cow;
use std::rc::Rc;
use url::Url;

#[test]
fn will_snapshot() {
  let snapshot = {
    let mut runtime = JsRuntimeForSnapshot::new(Default::default());
    runtime.execute_script_static("a.js", "a = 1 + 2").unwrap();
    runtime.snapshot()
  };

  let snapshot = Snapshot::JustCreated(snapshot);
  let mut runtime2 = JsRuntime::new(RuntimeOptions {
    startup_snapshot: Some(snapshot),
    ..Default::default()
  });
  runtime2
    .execute_script_static("check.js", "if (a != 3) throw Error('x')")
    .unwrap();
}

#[test]
fn will_snapshot2() {
  let startup_data = {
    let mut runtime = JsRuntimeForSnapshot::new(Default::default());
    runtime
      .execute_script_static("a.js", "let a = 1 + 2")
      .unwrap();
    runtime.snapshot()
  };

  let snapshot = Snapshot::JustCreated(startup_data);
  let mut runtime = JsRuntimeForSnapshot::new(RuntimeOptions {
    startup_snapshot: Some(snapshot),
    ..Default::default()
  });

  let startup_data = {
    runtime
      .execute_script_static("check_a.js", "if (a != 3) throw Error('x')")
      .unwrap();
    runtime.execute_script_static("b.js", "b = 2 + 3").unwrap();
    runtime.snapshot()
  };

  let snapshot = Snapshot::JustCreated(startup_data);
  {
    let mut runtime = JsRuntime::new(RuntimeOptions {
      startup_snapshot: Some(snapshot),
      ..Default::default()
    });
    runtime
      .execute_script_static("check_b.js", "if (b != 5) throw Error('x')")
      .unwrap();
    runtime
      .execute_script_static("check2.js", "if (!Deno.core) throw Error('x')")
      .unwrap();
  }
}

#[test]
fn test_snapshot_callbacks() {
  let snapshot = {
    let mut runtime = JsRuntimeForSnapshot::new(Default::default());
    runtime
      .execute_script_static(
        "a.js",
        r#"
        Deno.core.setMacrotaskCallback(() => {
          return true;
        });
        Deno.core.ops.op_set_format_exception_callback(()=> {
          return null;
        })
        Deno.core.setPromiseRejectCallback(() => {
          return false;
        });
        a = 1 + 2;
    "#,
      )
      .unwrap();
    runtime.snapshot()
  };

  let snapshot = Snapshot::JustCreated(snapshot);
  let mut runtime2 = JsRuntime::new(RuntimeOptions {
    startup_snapshot: Some(snapshot),
    ..Default::default()
  });
  runtime2
    .execute_script_static("check.js", "if (a != 3) throw Error('x')")
    .unwrap();
}

#[test]
fn test_from_boxed_snapshot() {
  let snapshot = {
    let mut runtime = JsRuntimeForSnapshot::new(Default::default());
    runtime.execute_script_static("a.js", "a = 1 + 2").unwrap();
    let snap: &[u8] = &runtime.snapshot();
    Vec::from(snap).into_boxed_slice()
  };

  let snapshot = Snapshot::Boxed(snapshot);
  let mut runtime2 = JsRuntime::new(RuntimeOptions {
    startup_snapshot: Some(snapshot),
    ..Default::default()
  });
  runtime2
    .execute_script_static("check.js", "if (a != 3) throw Error('x')")
    .unwrap();
}

#[test]
fn es_snapshot() {
  fn create_module(
    runtime: &mut JsRuntime,
    i: usize,
    main: bool,
  ) -> ModuleInfo {
    let specifier = crate::resolve_url(&format!("file:///{i}.js")).unwrap();
    let prev = i - 1;
    let source_code = format!(
      r#"
      import {{ f{prev} }} from "file:///{prev}.js";
      export function f{i}() {{ return f{prev}() }}
      "#
    )
    .into();

    let id = if main {
      futures::executor::block_on(
        runtime.load_main_module(&specifier, Some(source_code)),
      )
      .unwrap()
    } else {
      futures::executor::block_on(
        runtime.load_side_module(&specifier, Some(source_code)),
      )
      .unwrap()
    };
    assert_eq!(i, id);

    #[allow(clippy::let_underscore_future)]
    let _ = runtime.mod_evaluate(id);
    futures::executor::block_on(runtime.run_event_loop(false)).unwrap();

    ModuleInfo {
      id,
      main,
      name: specifier.into(),
      requests: vec![crate::modules::ModuleRequest {
        specifier: format!("file:///{prev}.js"),
        asserted_module_type: AssertedModuleType::JavaScriptOrWasm,
      }],
      module_type: AssertedModuleType::JavaScriptOrWasm,
    }
  }

  #[op2]
  #[string]
  fn op_test() -> Result<String, Error> {
    Ok(String::from("test"))
  }

  let mut runtime = JsRuntimeForSnapshot::new(RuntimeOptions {
    extensions: vec![Extension {
      name: "test_ext",
      ops: Cow::Borrowed(&[op_test::DECL]),
      ..Default::default()
    }],
    ..Default::default()
  });

  let specifier = crate::resolve_url("file:///0.js").unwrap();
  let source_code =
    ascii_str!(r#"export function f0() { return "hello world" }"#);
  let id = futures::executor::block_on(
    runtime.load_side_module(&specifier, Some(source_code)),
  )
  .unwrap();

  #[allow(clippy::let_underscore_future)]
  let _ = runtime.mod_evaluate(id);
  futures::executor::block_on(runtime.run_event_loop(false)).unwrap();

  let mut modules = vec![];
  modules.push(ModuleInfo {
    id,
    main: false,
    name: specifier.into(),
    requests: vec![],
    module_type: AssertedModuleType::JavaScriptOrWasm,
  });

  modules.extend((1..200).map(|i| create_module(&mut runtime, i, false)));

  runtime.module_map().assert_module_map(&modules);

  let snapshot = runtime.snapshot();

  let mut runtime2 = JsRuntimeForSnapshot::new(RuntimeOptions {
    startup_snapshot: Some(Snapshot::JustCreated(snapshot)),
    extensions: vec![Extension {
      name: "test_ext",
      ops: Cow::Borrowed(&[op_test::DECL]),
      ..Default::default()
    }],
    ..Default::default()
  });

  runtime2.module_map().assert_module_map(&modules);

  modules.extend((200..400).map(|i| create_module(&mut runtime2, i, false)));
  modules.push(create_module(&mut runtime2, 400, true));

  runtime2.module_map().assert_module_map(&modules);

  let snapshot2 = runtime2.snapshot();

  let mut runtime3 = JsRuntime::new(RuntimeOptions {
    startup_snapshot: Some(Snapshot::JustCreated(snapshot2)),
    extensions: vec![Extension {
      name: "test_ext",
      ops: Cow::Borrowed(&[op_test::DECL]),
      ..Default::default()
    }],
    ..Default::default()
  });

  runtime3.module_map().assert_module_map(&modules);

  let source_code = r#"(async () => {
    const mod = await import("file:///400.js");
    return mod.f400() + " " + Deno.core.ops.op_test();
  })();"#;
  let val = runtime3.execute_script_static(".", source_code).unwrap();
  let val = futures::executor::block_on(runtime3.resolve_value(val)).unwrap();
  {
    let scope = &mut runtime3.handle_scope();
    let value = v8::Local::new(scope, val);
    let str_ = value.to_string(scope).unwrap().to_rust_string_lossy(scope);
    assert_eq!(str_, "hello world test");
  }
}

pub(crate) fn generic_es_snapshot_without_runtime_module_loader(
  test_realms: bool,
) {
  let startup_data = {
    let extension = Extension {
      name: "module_snapshot",
      esm_files: Cow::Borrowed(&[ExtensionFileSource {
        specifier: "ext:module_snapshot/test.js",
        code: ExtensionFileSourceCode::IncludedInBinary(
          "globalThis.TEST = 'foo'; export const TEST = 'bar';",
        ),
      }]),
      esm_entry_point: Some("ext:module_snapshot/test.js"),
      ..Default::default()
    };

    let runtime = JsRuntimeForSnapshot::new(RuntimeOptions {
      extensions: vec![extension],
      ..Default::default()
    });

    runtime.snapshot()
  };

  let mut runtime;
  let realm;
  if test_realms {
    runtime = JsRuntime::new(RuntimeOptions {
      module_loader: Some(Rc::new(StaticModuleLoader::new([]))),
      startup_snapshot: Some(Snapshot::JustCreated(startup_data)),
      ..Default::default()
    });
    realm = runtime
      .create_realm(CreateRealmOptions {
        module_loader: None,
      })
      .unwrap();
  } else {
    runtime = JsRuntime::new(RuntimeOptions {
      module_loader: None,
      startup_snapshot: Some(Snapshot::JustCreated(startup_data)),
      ..Default::default()
    });
    realm = runtime.main_realm();
  }

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

  // We can still import a module that was registered manually
  let dyn_import_promise = realm
    .execute_script_static(
      runtime.v8_isolate(),
      "",
      "import('ext:module_snapshot/test.js')",
    )
    .unwrap();
  let dyn_import_result =
    futures::executor::block_on(runtime.resolve_value(dyn_import_promise));
  assert!(dyn_import_result.is_ok());

  // But not a new one
  let dyn_import_promise = realm
    .execute_script_static(
      runtime.v8_isolate(),
      "",
      "import('ext:module_snapshot/test2.js')",
    )
    .unwrap();
  let dyn_import_result =
    futures::executor::block_on(runtime.resolve_value(dyn_import_promise));
  assert!(dyn_import_result.is_err());
  assert_eq!(
    dyn_import_result.err().unwrap().to_string().as_str(),
    r#"Uncaught (in promise) TypeError: Module loading is not supported; attempted to load: "ext:module_snapshot/test2.js" from "(no referrer)""#
  );
}

#[test]
fn es_snapshot_without_runtime_module_loader() {
  generic_es_snapshot_without_runtime_module_loader(false)
}

pub(crate) fn generic_preserve_snapshotted_modules_test(
  test_snapshot: bool,
  test_realms: bool,
) {
  let extension = Extension {
    name: "module_snapshot",
    esm_files: Cow::Borrowed(&[
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
    ]),
    esm_entry_point: Some("test:not-preserved"),
    ..Default::default()
  };

  let loader = Rc::new(LoggingModuleLoader::new(NoopModuleLoader));

  let mut runtime = if test_snapshot {
    let snapshot_runtime = JsRuntimeForSnapshot::new(RuntimeOptions {
      extensions: vec![extension],
      ..Default::default()
    });
    let startup_data = snapshot_runtime.snapshot();

    JsRuntime::new(RuntimeOptions {
      module_loader: Some(loader.clone()),
      startup_snapshot: Some(Snapshot::JustCreated(startup_data)),
      preserve_snapshotted_modules: Some(&["test:preserved"]),
      ..Default::default()
    })
  } else {
    JsRuntime::new(RuntimeOptions {
      module_loader: Some(loader.clone()),
      extensions: vec![extension],
      preserve_snapshotted_modules: Some(&["test:preserved"]),
      ..Default::default()
    })
  };

  let realm = if test_realms {
    runtime
      .create_realm(CreateRealmOptions {
        module_loader: Some(loader.clone()),
      })
      .unwrap()
  } else {
    runtime.main_realm()
  };

  // We can't import "test:not-preserved"
  {
    assert_eq!(loader.log(), vec![]);
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
      "Uncaught (in promise) TypeError: Module loading is not supported; attempted to load: \"test:not-preserved\" from \"(no referrer)\""
    );
    // Ensure that we tried to load `test:not-preserved`
    assert_eq!(
      loader.log(),
      vec![Url::parse("test:not-preserved").unwrap()]
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

#[test]
fn preserve_snapshotted_modules() {
  generic_preserve_snapshotted_modules_test(true, false)
}

/// Test that `RuntimeOptions::preserve_snapshotted_modules` also works without
/// a snapshot.
#[test]
fn non_snapshot_preserve_snapshotted_modules() {
  generic_preserve_snapshotted_modules_test(false, false)
}
