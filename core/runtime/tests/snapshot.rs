// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use crate::extensions::Op;
use crate::module_specifier::ModuleSpecifier;
use crate::modules::AssertedModuleType;
use crate::modules::ModuleInfo;
use crate::modules::ModuleLoadId;
use crate::modules::ModuleLoader;
use crate::modules::ModuleSourceFuture;
use crate::modules::ModuleType;
use crate::modules::ResolutionKind;
use crate::modules::SymbolicModule;
use crate::*;
use anyhow::Error;
use deno_ops::op;
use std::pin::Pin;
use std::rc::Rc;

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
  #[derive(Default)]
  struct ModsLoader;

  impl ModuleLoader for ModsLoader {
    fn resolve(
      &self,
      specifier: &str,
      referrer: &str,
      _kind: ResolutionKind,
    ) -> Result<ModuleSpecifier, Error> {
      let s = crate::resolve_import(specifier, referrer).unwrap();
      Ok(s)
    }

    fn load(
      &self,
      _module_specifier: &ModuleSpecifier,
      _maybe_referrer: Option<&ModuleSpecifier>,
      _is_dyn_import: bool,
    ) -> Pin<Box<ModuleSourceFuture>> {
      eprintln!("load() should not be called");
      unreachable!()
    }
  }

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

  let loader = Rc::new(ModsLoader);
  let mut runtime = JsRuntimeForSnapshot::new(
    RuntimeOptions {
      module_loader: Some(loader.clone()),
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
      module_loader: Some(loader.clone()),
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
    module_loader: Some(loader),
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
