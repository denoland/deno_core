// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use crate::extensions::Op;
use crate::modules::ModuleInfo;
use crate::modules::RequestedModuleType;
use crate::runtime::NO_OF_BUILTIN_MODULES;
use crate::*;
use anyhow::Error;
use std::borrow::Cow;

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
        Deno.core.setUnhandledPromiseRejectionHandler(() => {
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
    assert_eq!(i + NO_OF_BUILTIN_MODULES, id);

    #[allow(clippy::let_underscore_future)]
    let _ = runtime.mod_evaluate(id);
    futures::executor::block_on(runtime.run_event_loop(Default::default()))
      .unwrap();

    ModuleInfo {
      id,
      main,
      name: specifier.into(),
      requests: vec![crate::modules::ModuleRequest {
        specifier: format!("file:///{prev}.js"),
        requested_module_type: RequestedModuleType::None,
      }],
      module_type: RequestedModuleType::None,
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
  futures::executor::block_on(runtime.run_event_loop(Default::default()))
    .unwrap();

  let mut modules = vec![];
  modules.push(ModuleInfo {
    id,
    main: false,
    name: specifier.into(),
    requests: vec![],
    module_type: RequestedModuleType::None,
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
  #[allow(deprecated)]
  let val = futures::executor::block_on(runtime3.resolve_value(val)).unwrap();
  {
    let scope = &mut runtime3.handle_scope();
    let value = v8::Local::new(scope, val);
    let str_ = value.to_string(scope).unwrap().to_rust_string_lossy(scope);
    assert_eq!(str_, "hello world test");
  }
}

#[test]
pub(crate) fn es_snapshot_without_runtime_module_loader() {
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

  let mut runtime = JsRuntime::new(RuntimeOptions {
    module_loader: None,
    startup_snapshot: Some(Snapshot::JustCreated(startup_data)),
    ..Default::default()
  });
  let realm = runtime.main_realm();

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

  // Dynamic imports of ext: from non-ext: modules are not allowed.
  let dyn_import_promise = realm
    .execute_script_static(
      runtime.v8_isolate(),
      "",
      "import('ext:module_snapshot/test.js')",
    )
    .unwrap();
  #[allow(deprecated)]
  let dyn_import_result =
    futures::executor::block_on(runtime.resolve_value(dyn_import_promise));
  assert_eq!(
    dyn_import_result.err().unwrap().to_string().as_str(),
    r#"Uncaught (in promise) TypeError: Importing ext: modules is only allowed from ext: and node: modules. Tried to import ext:module_snapshot/test.js from (no referrer)"#
  );

  // But not a new one
  let dyn_import_promise = realm
    .execute_script_static(
      runtime.v8_isolate(),
      "",
      "import('ext:module_snapshot/test2.js')",
    )
    .unwrap();
  #[allow(deprecated)]
  let dyn_import_result =
    futures::executor::block_on(runtime.resolve_value(dyn_import_promise));
  assert!(dyn_import_result.is_err());
  assert_eq!(
    dyn_import_result.err().unwrap().to_string().as_str(),
    r#"Uncaught (in promise) TypeError: Importing ext: modules is only allowed from ext: and node: modules. Tried to import ext:module_snapshot/test2.js from (no referrer)"#
  );
}
