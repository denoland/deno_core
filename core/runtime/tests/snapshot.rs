// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use crate::extensions::Op;
use crate::modules::AssertedModuleType;
use crate::modules::LoggingModuleLoader;
use crate::modules::ModuleInfo;
use crate::modules::ModuleLoadId;
use crate::modules::ModuleType;
use crate::modules::SymbolicModule;
use crate::*;
use anyhow::Error;
use deno_ops::op;
use std::rc::Rc;
use url::Url;

#[test]
fn will_snapshot() {
  let snapshot = {
    let mut runtime =
      JsRuntimeForSnapshot::new(Default::default(), Default::default());
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
    let mut runtime =
      JsRuntimeForSnapshot::new(Default::default(), Default::default());
    runtime
      .execute_script_static("a.js", "let a = 1 + 2")
      .unwrap();
    runtime.snapshot()
  };

  let snapshot = Snapshot::JustCreated(startup_data);
  let mut runtime = JsRuntimeForSnapshot::new(
    RuntimeOptions {
      startup_snapshot: Some(snapshot),
      ..Default::default()
    },
    Default::default(),
  );

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
    let mut runtime =
      JsRuntimeForSnapshot::new(Default::default(), Default::default());
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
    let mut runtime =
      JsRuntimeForSnapshot::new(Default::default(), Default::default());
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
      module_type: ModuleType::JavaScript,
    }
  }

  fn assert_module_map(runtime: &mut JsRuntime, modules: &Vec<ModuleInfo>) {
    let module_map_rc = runtime.module_map();
    let module_map = module_map_rc.borrow();
    assert_eq!(module_map.handles.len(), modules.len());
    assert_eq!(module_map.info.len(), modules.len());
    assert_eq!(
      module_map.by_name(AssertedModuleType::Json).len()
        + module_map
          .by_name(AssertedModuleType::JavaScriptOrWasm)
          .len(),
      modules.len()
    );

    assert_eq!(module_map.next_load_id, (modules.len() + 1) as ModuleLoadId);

    for info in modules {
      assert!(module_map.handles.get(info.id).is_some());
      assert_eq!(module_map.info.get(info.id).unwrap(), info);
      assert_eq!(
        module_map
          .by_name(AssertedModuleType::JavaScriptOrWasm)
          .get(&info.name)
          .unwrap(),
        &SymbolicModule::Mod(info.id)
      );
    }
  }

  #[op]
  fn op_test() -> Result<String, Error> {
    Ok(String::from("test"))
  }

  let mut runtime = JsRuntimeForSnapshot::new(
    RuntimeOptions {
      extensions: vec![Extension::builder("text_ext")
        .ops(vec![op_test::DECL])
        .build()],
      ..Default::default()
    },
    Default::default(),
  );

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
    module_type: ModuleType::JavaScript,
  });

  modules.extend((1..200).map(|i| create_module(&mut runtime, i, false)));

  assert_module_map(&mut runtime, &modules);

  let snapshot = runtime.snapshot();

  let mut runtime2 = JsRuntimeForSnapshot::new(
    RuntimeOptions {
      startup_snapshot: Some(Snapshot::JustCreated(snapshot)),
      extensions: vec![Extension::builder("text_ext")
        .ops(vec![op_test::DECL])
        .build()],
      ..Default::default()
    },
    Default::default(),
  );

  assert_module_map(&mut runtime2, &modules);

  modules.extend((200..400).map(|i| create_module(&mut runtime2, i, false)));
  modules.push(create_module(&mut runtime2, 400, true));

  assert_module_map(&mut runtime2, &modules);

  let snapshot2 = runtime2.snapshot();

  let mut runtime3 = JsRuntime::new(RuntimeOptions {
    startup_snapshot: Some(Snapshot::JustCreated(snapshot2)),
    extensions: vec![Extension::builder("text_ext")
      .ops(vec![op_test::DECL])
      .build()],
    ..Default::default()
  });

  assert_module_map(&mut runtime3, &modules);

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

#[test]
fn es_snapshot_without_runtime_module_loader() {
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

  let mut runtime = JsRuntime::new(RuntimeOptions {
    module_loader: None,
    startup_snapshot: Some(Snapshot::JustCreated(startup_data)),
    ..Default::default()
  });

  // Make sure the module was evaluated.
  {
    let scope = &mut runtime.handle_scope();
    let global_test: v8::Local<v8::String> =
      JsRuntime::eval(scope, "globalThis.TEST").unwrap();
    assert_eq!(
      serde_v8::to_utf8(global_test.to_string(scope).unwrap(), scope),
      String::from("foo"),
    );
  }

  // We can still import a module that was registered manually
  let dyn_import_promise = runtime
    .execute_script_static("", "import('ext:module_snapshot/test.js')")
    .unwrap();
  let dyn_import_result =
    futures::executor::block_on(runtime.resolve_value(dyn_import_promise));
  assert!(dyn_import_result.is_ok());

  // But not a new one
  let dyn_import_promise = runtime
    .execute_script_static("", "import('ext:module_snapshot/test2.js')")
    .unwrap();
  let dyn_import_result =
    futures::executor::block_on(runtime.resolve_value(dyn_import_promise));
  assert!(dyn_import_result.is_err());
  assert_eq!(
    dyn_import_result.err().unwrap().to_string().as_str(),
    r#"Uncaught TypeError: Module loading is not supported; attempted to load: "ext:module_snapshot/test2.js" from "(no referrer)""#
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

  let loader = Rc::new(LoggingModuleLoader::new(NoopModuleLoader));

  let mut runtime = JsRuntime::new(RuntimeOptions {
    module_loader: Some(loader.clone()),
    startup_snapshot: Some(Snapshot::JustCreated(startup_data)),
    preserve_snapshotted_modules: Some(&["test:preserved"]),
    ..Default::default()
  });

  // We can't import "test:not-preserved"
  {
    assert_eq!(loader.log(), vec![]);
    let dyn_import_promise = runtime
      .execute_script_static("", "import('test:not-preserved')")
      .unwrap();
    let dyn_import_result =
      futures::executor::block_on(runtime.resolve_value(dyn_import_promise));
    assert!(dyn_import_result.is_err());
    assert_eq!(
      dyn_import_result.err().unwrap().to_string().as_str(),
      "Uncaught TypeError: Module loading is not supported; attempted to load: \"test:not-preserved\" from \"(no referrer)\""
    );
    // Ensure that we tried to load `test:not-preserved`
    assert_eq!(
      loader.log(),
      vec![Url::parse("test:not-preserved").unwrap()]
    );
  }

  // But we can import "test:preserved"
  {
    let dyn_import_promise = runtime
      .execute_script_static(
        "",
        "import('test:preserved').then(module => module.TEST)",
      )
      .unwrap();
    let dyn_import_result =
      futures::executor::block_on(runtime.resolve_value(dyn_import_promise))
        .unwrap();
    let scope = &mut runtime.handle_scope();
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

  let loader = Rc::new(LoggingModuleLoader::new(NoopModuleLoader));

  let mut runtime = JsRuntime::new(RuntimeOptions {
    module_loader: Some(loader.clone()),
    extensions: vec![extension],
    preserve_snapshotted_modules: Some(&["test:preserved"]),
    ..Default::default()
  });

  // We can't import "test:not-preserved"
  {
    assert_eq!(loader.log(), vec![]);
    let dyn_import_promise = runtime
      .execute_script_static("", "import('test:not-preserved')")
      .unwrap();
    let dyn_import_result =
      futures::executor::block_on(runtime.resolve_value(dyn_import_promise));
    assert!(dyn_import_result.is_err());
    assert_eq!(
      dyn_import_result.err().unwrap().to_string().as_str(),
      "Uncaught TypeError: Module loading is not supported; attempted to load: \"test:not-preserved\" from \"(no referrer)\""
    );
    // Ensure that we tried to load `test:not-preserved`
    assert_eq!(
      loader.log(),
      vec![Url::parse("test:not-preserved").unwrap()]
    );
  }

  // But we can import "test:preserved"
  {
    let dyn_import_promise = runtime
      .execute_script_static(
        "",
        "import('test:preserved').then(module => module.TEST)",
      )
      .unwrap();
    let dyn_import_result =
      futures::executor::block_on(runtime.resolve_value(dyn_import_promise))
        .unwrap();
    let scope = &mut runtime.handle_scope();
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
