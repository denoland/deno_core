// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use crate::ascii_str;
use crate::error::exception_to_err_result;
use crate::modules::loaders::ModuleLoadEventCounts;
use crate::modules::loaders::TestingModuleLoader;
use crate::modules::loaders::*;
use crate::modules::ModuleError;
use crate::modules::ModuleRequest;
use crate::modules::ModuleSourceCode;
use crate::modules::RequestedModuleType;
use crate::resolve_import;
use crate::resolve_url;
use crate::runtime::JsRuntime;
use crate::runtime::JsRuntimeForSnapshot;
use crate::FastString;
use crate::ModuleCode;
use crate::ModuleSource;
use crate::ModuleSourceFuture;
use crate::ModuleSpecifier;
use crate::ModuleType;
use crate::OpState;
use crate::ResolutionKind;
use crate::RuntimeOptions;
use crate::Snapshot;
use anyhow::bail;
use anyhow::Error;
use deno_ops::op2;
use futures::future::poll_fn;
use futures::future::FutureExt;
use parking_lot::Mutex;
use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::task::Context;
use std::task::Poll;
use tokio::sync::Barrier;
use tokio::task::LocalSet;
use url::Url;

// deno_ops macros generate code assuming deno_core in scope.
use crate::deno_core;

#[derive(Default)]
struct MockLoader {
  pub loads: Arc<Mutex<Vec<String>>>,
}

impl MockLoader {
  fn new() -> Rc<Self> {
    Default::default()
  }
}

fn mock_source_code(url: &str) -> Option<(&'static str, &'static str)> {
  const A_SRC: &str = r#"
import { b } from "/b.js";
import { c } from "/c.js";
if (b() != 'b') throw Error();
if (c() != 'c') throw Error();
if (!import.meta.main) throw Error();
if (import.meta.url != 'file:///a.js') throw Error();
"#;

  const B_SRC: &str = r#"
import { c } from "/c.js";
if (c() != 'c') throw Error();
export function b() { return 'b'; }
if (import.meta.main) throw Error();
if (import.meta.url != 'file:///b.js') throw Error();
"#;

  const C_SRC: &str = r#"
import { d } from "/d.js";
export function c() { return 'c'; }
if (d() != 'd') throw Error();
if (import.meta.main) throw Error();
if (import.meta.url != 'file:///c.js') throw Error();
"#;

  const D_SRC: &str = r#"
export function d() { return 'd'; }
if (import.meta.main) throw Error();
if (import.meta.url != 'file:///d.js') throw Error();
"#;

  const CIRCULAR1_SRC: &str = r#"
import "/circular2.js";
Deno.core.print("circular1");
"#;

  const CIRCULAR2_SRC: &str = r#"
import "/circular3.js";
Deno.core.print("circular2");
"#;

  const CIRCULAR3_SRC: &str = r#"
import "/circular1.js";
import "/circular2.js";
Deno.core.print("circular3");
"#;

  const REDIRECT1_SRC: &str = r#"
import "./redirect2.js";
Deno.core.print("redirect1");
"#;

  const REDIRECT2_SRC: &str = r#"
import "./redirect3.js";
Deno.core.print("redirect2");
"#;

  const REDIRECT3_SRC: &str = r#"Deno.core.print("redirect3");"#;

  const MAIN_SRC: &str = r#"
// never_ready.js never loads.
import "/never_ready.js";
// slow.js resolves after one tick.
import "/slow.js";
"#;

  const SLOW_SRC: &str = r#"
// Circular import of never_ready.js
// Does this trigger two ModuleLoader calls? It shouldn't.
import "/never_ready.js";
import "/a.js";
"#;

  const BAD_IMPORT_SRC: &str = r#"import "foo";"#;

  // (code, real_module_name)
  let spec: Vec<&str> = url.split("file://").collect();
  match spec[1] {
    "/a.js" => Some((A_SRC, "file:///a.js")),
    "/b.js" => Some((B_SRC, "file:///b.js")),
    "/c.js" => Some((C_SRC, "file:///c.js")),
    "/d.js" => Some((D_SRC, "file:///d.js")),
    "/circular1.js" => Some((CIRCULAR1_SRC, "file:///circular1.js")),
    "/circular2.js" => Some((CIRCULAR2_SRC, "file:///circular2.js")),
    "/circular3.js" => Some((CIRCULAR3_SRC, "file:///circular3.js")),
    "/redirect1.js" => Some((REDIRECT1_SRC, "file:///redirect1.js")),
    // pretend redirect - real module name is different than one requested
    "/redirect2.js" => Some((REDIRECT2_SRC, "file:///dir/redirect2.js")),
    "/dir/redirect3.js" => Some((REDIRECT3_SRC, "file:///redirect3.js")),
    "/slow.js" => Some((SLOW_SRC, "file:///slow.js")),
    "/never_ready.js" => {
      Some(("should never be Ready", "file:///never_ready.js"))
    }
    "/main.js" => Some((MAIN_SRC, "file:///main.js")),
    "/bad_import.js" => Some((BAD_IMPORT_SRC, "file:///bad_import.js")),
    // deliberately empty code.
    "/main_with_code.js" => Some(("", "file:///main_with_code.js")),
    _ => None,
  }
}

#[derive(Debug, PartialEq)]
enum MockError {
  ResolveErr,
  LoadErr,
}

impl fmt::Display for MockError {
  fn fmt(&self, _f: &mut fmt::Formatter) -> fmt::Result {
    unimplemented!()
  }
}

impl std::error::Error for MockError {
  fn cause(&self) -> Option<&dyn std::error::Error> {
    unimplemented!()
  }
}

struct DelayedSourceCodeFuture {
  url: String,
  counter: u32,
}

impl Future for DelayedSourceCodeFuture {
  type Output = Result<ModuleSource, Error>;

  fn poll(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
    let inner = self.get_mut();
    inner.counter += 1;
    if inner.url == "file:///never_ready.js" {
      return Poll::Pending;
    }
    if inner.url == "file:///slow.js" && inner.counter < 2 {
      // TODO(ry) Hopefully in the future we can remove current task
      // notification.
      cx.waker().wake_by_ref();
      return Poll::Pending;
    }
    match mock_source_code(&inner.url) {
      Some(src) => Poll::Ready(Ok(ModuleSource::for_test_with_redirect(
        src.0,
        inner.url.as_str(),
        src.1,
      ))),
      None => Poll::Ready(Err(MockError::LoadErr.into())),
    }
  }
}

impl ModuleLoader for MockLoader {
  fn resolve(
    &self,
    specifier: &str,
    referrer: &str,
    _kind: ResolutionKind,
  ) -> Result<ModuleSpecifier, Error> {
    let referrer = if referrer == "." {
      "file:///"
    } else {
      referrer
    };

    let output_specifier = match resolve_import(specifier, referrer) {
      Ok(specifier) => specifier,
      Err(..) => return Err(MockError::ResolveErr.into()),
    };

    if mock_source_code(output_specifier.as_ref()).is_some() {
      Ok(output_specifier)
    } else {
      Err(MockError::ResolveErr.into())
    }
  }

  fn load(
    &self,
    module_specifier: &ModuleSpecifier,
    _maybe_referrer: Option<&ModuleSpecifier>,
    _is_dyn_import: bool,
  ) -> Pin<Box<ModuleSourceFuture>> {
    let mut loads = self.loads.lock();
    loads.push(module_specifier.to_string());
    let url = module_specifier.to_string();
    DelayedSourceCodeFuture { url, counter: 0 }.boxed()
  }
}

#[test]
fn test_recursive_load() {
  let loader = MockLoader::new();
  let loads = loader.loads.clone();
  let mut runtime = JsRuntime::new(RuntimeOptions {
    module_loader: Some(loader),
    ..Default::default()
  });
  let spec = resolve_url("file:///a.js").unwrap();
  let a_id_fut = runtime.load_main_module(&spec, None);
  let a_id = futures::executor::block_on(a_id_fut).unwrap();

  #[allow(clippy::let_underscore_future)]
  let _ = runtime.mod_evaluate(a_id);
  futures::executor::block_on(runtime.run_event_loop(Default::default()))
    .unwrap();
  let l = loads.lock();
  assert_eq!(
    l.to_vec(),
    vec![
      "file:///a.js",
      "file:///b.js",
      "file:///c.js",
      "file:///d.js"
    ]
  );

  let module_map_rc = runtime.module_map();
  let modules = module_map_rc;

  assert_eq!(
    modules.get_id("file:///a.js", RequestedModuleType::None),
    Some(a_id)
  );
  let b_id = modules
    .get_id("file:///b.js", RequestedModuleType::None)
    .unwrap();
  let c_id = modules
    .get_id("file:///c.js", RequestedModuleType::None)
    .unwrap();
  let d_id = modules
    .get_id("file:///d.js", RequestedModuleType::None)
    .unwrap();
  assert_eq!(
    modules.get_requested_modules(a_id),
    Some(vec![
      ModuleRequest {
        specifier: "file:///b.js".to_string(),
        requested_module_type: RequestedModuleType::None,
      },
      ModuleRequest {
        specifier: "file:///c.js".to_string(),
        requested_module_type: RequestedModuleType::None,
      },
    ])
  );
  assert_eq!(
    modules.get_requested_modules(b_id),
    Some(vec![ModuleRequest {
      specifier: "file:///c.js".to_string(),
      requested_module_type: RequestedModuleType::None,
    },])
  );
  assert_eq!(
    modules.get_requested_modules(c_id),
    Some(vec![ModuleRequest {
      specifier: "file:///d.js".to_string(),
      requested_module_type: RequestedModuleType::None,
    },])
  );
  assert_eq!(modules.get_requested_modules(d_id), Some(vec![]));
}

#[test]
fn test_mods() {
  let loader = Rc::new(TestingModuleLoader::new(NoopModuleLoader));
  static DISPATCH_COUNT: AtomicUsize = AtomicUsize::new(0);

  #[op2(fast)]
  fn op_test(control: u8) -> u8 {
    DISPATCH_COUNT.fetch_add(1, Ordering::Relaxed);
    assert_eq!(control, 42);
    43
  }

  deno_core::extension!(test_ext, ops = [op_test]);

  let mut runtime = JsRuntime::new(RuntimeOptions {
    extensions: vec![test_ext::init_ops()],
    module_loader: Some(loader.clone()),
    ..Default::default()
  });

  runtime
    .execute_script_static(
      "setup.js",
      r#"
      function assert(cond) {
        if (!cond) {
          throw Error("assert");
        }
      }
      "#,
    )
    .unwrap();

  assert_eq!(DISPATCH_COUNT.load(Ordering::Relaxed), 0);

  let module_map = runtime.module_map().clone();

  let (mod_a, mod_b) = {
    let scope = &mut runtime.handle_scope();
    let specifier_a = ascii_str!("file:///a.js");
    let mod_a = module_map
      .new_es_module(
        scope,
        true,
        specifier_a,
        ascii_str!(
          r#"
        import { b } from './b.js'
        if (b() != 'b') throw Error();
        let control = 42;
        Deno.core.ops.op_test(control);
      "#
        ),
        false,
      )
      .unwrap();

    assert_eq!(DISPATCH_COUNT.load(Ordering::Relaxed), 0);
    let imports = module_map.get_requested_modules(mod_a);
    assert_eq!(
      imports,
      Some(vec![ModuleRequest {
        specifier: "file:///b.js".to_string(),
        requested_module_type: RequestedModuleType::None,
      },])
    );

    let mod_b = module_map
      .new_es_module(
        scope,
        false,
        ascii_str!("file:///b.js"),
        ascii_str!("export function b() { return 'b' }"),
        false,
      )
      .unwrap();
    let imports = module_map.get_requested_modules(mod_b).unwrap();
    assert_eq!(imports.len(), 0);
    (mod_a, mod_b)
  };

  runtime.instantiate_module(mod_b).unwrap();
  assert_eq!(DISPATCH_COUNT.load(Ordering::Relaxed), 0);
  assert_eq!(loader.counts(), ModuleLoadEventCounts::new(1, 0, 0));

  runtime.instantiate_module(mod_a).unwrap();
  assert_eq!(DISPATCH_COUNT.load(Ordering::Relaxed), 0);

  #[allow(clippy::let_underscore_future)]
  let _ = runtime.mod_evaluate(mod_a);
  assert_eq!(DISPATCH_COUNT.load(Ordering::Relaxed), 1);
}

#[test]
fn test_lazy_loaded_esm() {
  deno_core::extension!(test_ext, lazy_loaded_esm = [dir "modules/testdata", "lazy_loaded.js"]);

  let mut runtime = JsRuntime::new(RuntimeOptions {
    extensions: vec![test_ext::init_ops_and_esm()],
    ..Default::default()
  });

  runtime
    .execute_script_static(
      "setup.js",
      r#"
      Deno.core.print("1\n");
      const module = Deno.core.ops.op_lazy_load_esm("ext:test_ext/lazy_loaded.js");
      module.blah("hello\n");
      Deno.core.print(`${JSON.stringify(module)}\n`);
      const module1 = Deno.core.ops.op_lazy_load_esm("ext:test_ext/lazy_loaded.js");
      if (module !== module1) throw new Error("should return the same error");
      "#,
    )
    .unwrap();
}

#[test]
fn test_json_module() {
  let loader = Rc::new(TestingModuleLoader::new(StaticModuleLoader::new([])));
  let mut runtime = JsRuntime::new(RuntimeOptions {
    module_loader: Some(loader.clone()),
    ..Default::default()
  });

  runtime
    .execute_script_static(
      "setup.js",
      r#"
        function assert(cond) {
          if (!cond) {
            throw Error("assert");
          }
        }
        "#,
    )
    .unwrap();

  let module_map = runtime.module_map().clone();

  let (mod_a, mod_b, mod_c) = {
    let scope = &mut runtime.handle_scope();
    let specifier_a = ascii_str!("file:///a.js");
    let specifier_b = ascii_str!("file:///b.js");
    let mod_a = module_map
      .new_es_module(
        scope,
        true,
        specifier_a,
        ascii_str!(
          r#"
          import jsonData from './c.json' assert {type: "json"};
          assert(jsonData.a == "b");
          assert(jsonData.c.d == 10);
        "#
        ),
        false,
      )
      .unwrap();

    let imports = module_map.get_requested_modules(mod_a);
    assert_eq!(
      imports,
      Some(vec![ModuleRequest {
        specifier: "file:///c.json".to_string(),
        requested_module_type: RequestedModuleType::Json,
      },])
    );

    let mod_b = module_map
      .new_es_module(
        scope,
        false,
        specifier_b,
        ascii_str!(
          r#"
          import jsonData from './c.json' with {type: "json"};
          assert(jsonData.a == "b");
          assert(jsonData.c.d == 10);
        "#
        ),
        false,
      )
      .unwrap();

    let imports = module_map.get_requested_modules(mod_b);
    assert_eq!(
      imports,
      Some(vec![ModuleRequest {
        specifier: "file:///c.json".to_string(),
        requested_module_type: RequestedModuleType::Json,
      },])
    );

    let mod_c = module_map
      .new_json_module(
        scope,
        ascii_str!("file:///c.json"),
        ascii_str!("{\"a\": \"b\", \"c\": {\"d\": 10}}"),
      )
      .unwrap();
    let imports = module_map.get_requested_modules(mod_c).unwrap();
    assert_eq!(imports.len(), 0);
    (mod_a, mod_b, mod_c)
  };

  runtime.instantiate_module(mod_c).unwrap();
  assert_eq!(loader.counts(), ModuleLoadEventCounts::new(2, 0, 0));

  runtime.instantiate_module(mod_a).unwrap();

  let receiver = runtime.mod_evaluate(mod_a);
  futures::executor::block_on(runtime.run_event_loop(Default::default()))
    .unwrap();
  futures::executor::block_on(receiver).unwrap();

  runtime.instantiate_module(mod_b).unwrap();

  let receiver = runtime.mod_evaluate(mod_b);
  futures::executor::block_on(runtime.run_event_loop(Default::default()))
    .unwrap();
  futures::executor::block_on(receiver).unwrap();
}

#[test]
fn test_validate_import_attributes() {
  fn validate_import_attrs(
    scope: &mut v8::HandleScope,
    _attrs: &HashMap<String, String>,
  ) {
    let msg = v8::String::new(scope, "boom!").unwrap();
    let ex = v8::Exception::type_error(scope, msg);
    scope.throw_exception(ex);
  }

  let loader = Rc::new(TestingModuleLoader::new(StaticModuleLoader::new([])));
  let mut runtime = JsRuntime::new(RuntimeOptions {
    module_loader: Some(loader.clone()),
    validate_import_attributes_cb: Some(Box::new(validate_import_attrs)),
    ..Default::default()
  });

  let module_map = runtime.module_map().clone();

  {
    let scope = &mut runtime.handle_scope();
    let specifier_a = ascii_str!("file:///a.js");
    let module_err = module_map
      .new_es_module(
        scope,
        true,
        specifier_a,
        ascii_str!(
          r#"
          import jsonData from './c.json' with {foo: "bar"};
        "#
        ),
        false,
      )
      .unwrap_err();

    let ModuleError::Exception(exc) = module_err else {
      unreachable!();
    };
    let exception = v8::Local::new(scope, exc);
    let err =
      exception_to_err_result::<()>(scope, exception, false, true).unwrap_err();
    assert_eq!(err.to_string(), "Uncaught TypeError: boom!");
  }
}

#[test]
fn test_validate_import_attributes_default() {
  let loader = Rc::new(TestingModuleLoader::new(StaticModuleLoader::new([])));
  let mut runtime = JsRuntime::new(RuntimeOptions {
    module_loader: Some(loader.clone()),
    ..Default::default()
  });

  let module_map_rc = runtime.module_map().clone();

  {
    let scope = &mut runtime.handle_scope();
    let specifier_a = ascii_str!("file:///a.js");
    let module_err = module_map_rc
      .new_es_module(
        scope,
        true,
        specifier_a,
        ascii_str!(
          r#"
          import jsonData from './c.json' with {foo: "bar"};
        "#
        ),
        false,
      )
      .unwrap_err();

    let ModuleError::Exception(exc) = module_err else {
      unreachable!();
    };
    let exception = v8::Local::new(scope, exc);
    let err =
      exception_to_err_result::<()>(scope, exception, false, true).unwrap_err();
    assert_eq!(
      err.to_string(),
      "Uncaught TypeError: \"foo\" attribute is not supported."
    );
  }

  {
    let scope = &mut runtime.handle_scope();
    let specifier_a = ascii_str!("file:///a.js");
    let module_err = module_map_rc
      .new_es_module(
        scope,
        true,
        specifier_a,
        ascii_str!(
          r#"
          import jsonData from './c.json' with {type: "bar"};
        "#
        ),
        false,
      )
      .unwrap_err();

    let ModuleError::Exception(exc) = module_err else {
      unreachable!();
    };
    let exception = v8::Local::new(scope, exc);
    let err =
      exception_to_err_result::<()>(scope, exception, false, true).unwrap_err();
    assert_eq!(
      err.to_string(),
      "Uncaught TypeError: \"bar\" is not a valid module type."
    );
  }
}

#[tokio::test]
async fn dyn_import_op() {
  #[op2(async)]
  async fn op_test() {
    tokio::task::yield_now().await;
  }

  #[op2(async)]
  async fn op_wait(state: Rc<RefCell<OpState>>) {
    eprintln!("op_wait!");
    let barrier: Rc<Barrier> = state.borrow().borrow::<Rc<Barrier>>().clone();
    barrier.wait().await;
  }

  deno_core::extension!(test_ext,
    ops = [op_test, op_wait],
    options = { count: Barrier },
    state = |state, options| {
      state.put(Rc::new(options.count));
    },
  );

  let loader = Rc::new(TestingModuleLoader::new(StaticModuleLoader::new([
    (Url::parse("file:///main.js").unwrap(), ascii_str!("const { op_wait } = Deno.core.ensureFastOps(); (async () => { await import('./dynamic.js'); await op_wait(); })();")),
    (Url::parse("file:///dynamic.js").unwrap(), ascii_str!("const { op_test } = Deno.core.ensureFastOps(); await op_test();")),
  ])));
  let barrier = Barrier::new(2);
  let mut runtime = JsRuntime::new(RuntimeOptions {
    module_loader: Some(loader.clone()),
    extensions: vec![test_ext::init_ops(barrier)],
    ..Default::default()
  });
  let id = runtime
    .load_main_module(
      &Url::parse("file:///root.js").unwrap(),
      Some(ascii_str!(
        "import 'file:///main.js'; const { op_wait } = Deno.core.ensureFastOps(); await op_wait();"
      )),
    )
    .await
    .unwrap();
  let f = runtime.mod_evaluate(id);
  poll_fn(|cx| runtime.poll_event_loop(cx, Default::default()))
    .await
    .unwrap();
  _ = f.await;
}

#[tokio::test]
async fn dyn_import_err() {
  let loader = Rc::new(TestingModuleLoader::new(StaticModuleLoader::new([])));
  let mut runtime = JsRuntime::new(RuntimeOptions {
    module_loader: Some(loader.clone()),
    ..Default::default()
  });

  // Test an erroneous dynamic import where the specified module isn't found.
  poll_fn(move |cx| {
    runtime
      .execute_script_static(
        "file:///dyn_import2.js",
        r#"
      (async () => {
        await import("/foo.js");
      })();
      "#,
      )
      .unwrap();

    // We should get an error here.
    let result = runtime.poll_event_loop(cx, Default::default());
    assert!(matches!(result, Poll::Ready(Err(_))));
    assert_eq!(loader.counts(), ModuleLoadEventCounts::new(4, 1, 1));
    Poll::Ready(())
  })
  .await;
}

#[tokio::test]
async fn dyn_import_ok() {
  let loader = Rc::new(TestingModuleLoader::new(StaticModuleLoader::with(
    Url::parse("file:///b.js").unwrap(),
    ascii_str!("export function b() { return 'b' }"),
  )));
  let mut runtime = JsRuntime::new(RuntimeOptions {
    module_loader: Some(loader.clone()),
    ..Default::default()
  });
  poll_fn(move |cx| {
    // Dynamically import mod_b
    runtime
      .execute_script_static(
        "file:///dyn_import3.js",
        r#"
        (async () => {
          let mod = await import("./b.js");
          if (mod.b() !== 'b') {
            throw Error("bad1");
          }
          // And again!
          mod = await import("./b.js");
          if (mod.b() !== 'b') {
            throw Error("bad2");
          }
        })();
        "#,
      )
      .unwrap();

    assert!(matches!(
      runtime.poll_event_loop(cx, Default::default()),
      Poll::Ready(Ok(_))
    ));
    assert_eq!(loader.counts(), ModuleLoadEventCounts::new(7, 1, 1));
    assert!(matches!(
      runtime.poll_event_loop(cx, Default::default()),
      Poll::Ready(Ok(_))
    ));
    assert_eq!(loader.counts(), ModuleLoadEventCounts::new(7, 1, 1));
    Poll::Ready(())
  })
  .await;
}

#[tokio::test]
async fn dyn_import_borrow_mut_error() {
  // https://github.com/denoland/deno/issues/6054
  let loader = Rc::new(TestingModuleLoader::new(StaticModuleLoader::with(
    Url::parse("file:///b.js").unwrap(),
    ascii_str!("export function b() { return 'b' }"),
  )));
  let mut runtime = JsRuntime::new(RuntimeOptions {
    module_loader: Some(loader.clone()),
    ..Default::default()
  });

  poll_fn(move |cx| {
    runtime
      .execute_script_static(
        "file:///dyn_import3.js",
        r#"
        (async () => {
          let mod = await import("./b.js");
          if (mod.b() !== 'b') {
            throw Error("bad");
          }
        })();
        "#,
      )
      .unwrap();
    // TODO(mmastrac): These checks here are pretty broken and make a lot of assumptions
    // about the ordering of resolution and events. If these break, we can remove them.

    // Old comments that are likely wrong:
    // First poll runs `prepare_load` hook.
    let _ = runtime.poll_event_loop(cx, Default::default());
    assert_eq!(loader.counts(), ModuleLoadEventCounts::new(4, 1, 1));
    // Second poll triggers error
    let _ = runtime.poll_event_loop(cx, Default::default());
    assert_eq!(loader.counts(), ModuleLoadEventCounts::new(4, 1, 1));
    Poll::Ready(())
  })
  .await;
}

#[test]
fn test_circular_load() {
  let loader = MockLoader::new();
  let loads = loader.loads.clone();
  let mut runtime = JsRuntime::new(RuntimeOptions {
    module_loader: Some(loader),
    ..Default::default()
  });

  let fut = async move {
    let spec = resolve_url("file:///circular1.js").unwrap();
    let result = runtime.load_main_module(&spec, None).await;
    assert!(result.is_ok());
    let circular1_id = result.unwrap();
    #[allow(clippy::let_underscore_future)]
    let _ = runtime.mod_evaluate(circular1_id);
    runtime.run_event_loop(Default::default()).await.unwrap();

    let l = loads.lock();
    assert_eq!(
      l.to_vec(),
      vec![
        "file:///circular1.js",
        "file:///circular2.js",
        "file:///circular3.js"
      ]
    );

    let module_map_rc = runtime.module_map();
    let modules = module_map_rc;

    assert_eq!(
      modules.get_id("file:///circular1.js", RequestedModuleType::None),
      Some(circular1_id)
    );
    let circular2_id = modules
      .get_id("file:///circular2.js", RequestedModuleType::None)
      .unwrap();

    assert_eq!(
      modules.get_requested_modules(circular1_id),
      Some(vec![ModuleRequest {
        specifier: "file:///circular2.js".to_string(),
        requested_module_type: RequestedModuleType::None,
      }])
    );

    assert_eq!(
      modules.get_requested_modules(circular2_id),
      Some(vec![ModuleRequest {
        specifier: "file:///circular3.js".to_string(),
        requested_module_type: RequestedModuleType::None,
      }])
    );

    assert!(modules
      .get_id("file:///circular3.js", RequestedModuleType::None)
      .is_some());
    let circular3_id = modules
      .get_id("file:///circular3.js", RequestedModuleType::None)
      .unwrap();
    assert_eq!(
      modules.get_requested_modules(circular3_id),
      Some(vec![
        ModuleRequest {
          specifier: "file:///circular1.js".to_string(),
          requested_module_type: RequestedModuleType::None,
        },
        ModuleRequest {
          specifier: "file:///circular2.js".to_string(),
          requested_module_type: RequestedModuleType::None,
        }
      ])
    );
  }
  .boxed_local();

  futures::executor::block_on(fut);
}

#[test]
fn test_redirect_load() {
  let loader = MockLoader::new();
  let loads = loader.loads.clone();
  let mut runtime = JsRuntime::new(RuntimeOptions {
    module_loader: Some(loader),
    ..Default::default()
  });

  let fut =
    async move {
      let spec = resolve_url("file:///redirect1.js").unwrap();
      let result = runtime.load_main_module(&spec, None).await;
      assert!(result.is_ok());
      let redirect1_id = result.unwrap();
      #[allow(clippy::let_underscore_future)]
      let _ = runtime.mod_evaluate(redirect1_id);
      runtime.run_event_loop(Default::default()).await.unwrap();
      let l = loads.lock();
      assert_eq!(
        l.to_vec(),
        vec![
          "file:///redirect1.js",
          "file:///redirect2.js",
          "file:///dir/redirect3.js"
        ]
      );

      let module_map_rc = runtime.module_map();
      let modules = module_map_rc;

      assert_eq!(
        modules.get_id("file:///redirect1.js", RequestedModuleType::None),
        Some(redirect1_id)
      );

      let redirect2_id = modules
        .get_id("file:///dir/redirect2.js", RequestedModuleType::None)
        .unwrap();
      assert!(
        modules.is_alias("file:///redirect2.js", RequestedModuleType::None)
      );
      assert!(!modules
        .is_alias("file:///dir/redirect2.js", RequestedModuleType::None));
      assert_eq!(
        modules.get_id("file:///redirect2.js", RequestedModuleType::None),
        Some(redirect2_id)
      );

      let redirect3_id = modules
        .get_id("file:///redirect3.js", RequestedModuleType::None)
        .unwrap();
      assert!(
        modules.is_alias("file:///dir/redirect3.js", RequestedModuleType::None)
      );
      assert!(
        !modules.is_alias("file:///redirect3.js", RequestedModuleType::None)
      );
      assert_eq!(
        modules.get_id("file:///dir/redirect3.js", RequestedModuleType::None),
        Some(redirect3_id)
      );
    }
    .boxed_local();

  futures::executor::block_on(fut);
}

#[tokio::test]
async fn slow_never_ready_modules() {
  let loader = MockLoader::new();
  let loads = loader.loads.clone();
  let mut runtime = JsRuntime::new(RuntimeOptions {
    module_loader: Some(loader),
    ..Default::default()
  });

  poll_fn(move |cx| {
    let spec = resolve_url("file:///main.js").unwrap();
    let mut recursive_load =
      runtime.load_main_module(&spec, None).boxed_local();

    let result = recursive_load.poll_unpin(cx);
    assert!(result.is_pending());

    // TODO(ry) Arguably the first time we poll only the following modules
    // should be loaded:
    //      "file:///main.js",
    //      "file:///never_ready.js",
    //      "file:///slow.js"
    // But due to current task notification in DelayedSourceCodeFuture they
    // all get loaded in a single poll.

    for _ in 0..10 {
      let result = recursive_load.poll_unpin(cx);
      assert!(result.is_pending());
      let l = loads.lock();
      assert_eq!(
        l.to_vec(),
        vec![
          "file:///main.js",
          "file:///never_ready.js",
          "file:///slow.js",
          "file:///a.js",
          "file:///b.js",
          "file:///c.js",
          "file:///d.js"
        ]
      );
    }
    Poll::Ready(())
  })
  .await;
}

#[tokio::test]
async fn loader_disappears_after_error() {
  let loader = MockLoader::new();
  let mut runtime = JsRuntime::new(RuntimeOptions {
    module_loader: Some(loader),
    ..Default::default()
  });

  let spec = resolve_url("file:///bad_import.js").unwrap();
  let result = runtime.load_main_module(&spec, None).await;
  let err = result.unwrap_err();
  assert_eq!(
    err.downcast_ref::<MockError>().unwrap(),
    &MockError::ResolveErr
  );
}

#[test]
fn recursive_load_main_with_code() {
  const MAIN_WITH_CODE_SRC: FastString = ascii_str!(
    r#"
import { b } from "/b.js";
import { c } from "/c.js";
if (b() != 'b') throw Error();
if (c() != 'c') throw Error();
if (!import.meta.main) throw Error();
if (import.meta.url != 'file:///main_with_code.js') throw Error();
"#
  );

  let loader = MockLoader::new();
  let loads = loader.loads.clone();
  let mut runtime = JsRuntime::new(RuntimeOptions {
    module_loader: Some(loader),
    ..Default::default()
  });
  // In default resolution code should be empty.
  // Instead we explicitly pass in our own code.
  // The behavior should be very similar to /a.js.
  let spec = resolve_url("file:///main_with_code.js").unwrap();
  let main_id_fut = runtime
    .load_main_module(&spec, Some(MAIN_WITH_CODE_SRC))
    .boxed_local();
  let main_id = futures::executor::block_on(main_id_fut).unwrap();

  #[allow(clippy::let_underscore_future)]
  let _ = runtime.mod_evaluate(main_id);
  futures::executor::block_on(runtime.run_event_loop(Default::default()))
    .unwrap();

  let l = loads.lock();
  assert_eq!(
    l.to_vec(),
    vec!["file:///b.js", "file:///c.js", "file:///d.js"]
  );

  let module_map_rc = runtime.module_map();
  let modules = module_map_rc;

  assert_eq!(
    modules.get_id("file:///main_with_code.js", RequestedModuleType::None),
    Some(main_id)
  );
  let b_id = modules
    .get_id("file:///b.js", RequestedModuleType::None)
    .unwrap();
  let c_id = modules
    .get_id("file:///c.js", RequestedModuleType::None)
    .unwrap();
  let d_id = modules
    .get_id("file:///d.js", RequestedModuleType::None)
    .unwrap();

  assert_eq!(
    modules.get_requested_modules(main_id),
    Some(vec![
      ModuleRequest {
        specifier: "file:///b.js".to_string(),
        requested_module_type: RequestedModuleType::None,
      },
      ModuleRequest {
        specifier: "file:///c.js".to_string(),
        requested_module_type: RequestedModuleType::None,
      }
    ])
  );
  assert_eq!(
    modules.get_requested_modules(b_id),
    Some(vec![ModuleRequest {
      specifier: "file:///c.js".to_string(),
      requested_module_type: RequestedModuleType::None,
    }])
  );
  assert_eq!(
    modules.get_requested_modules(c_id),
    Some(vec![ModuleRequest {
      specifier: "file:///d.js".to_string(),
      requested_module_type: RequestedModuleType::None,
    }])
  );
  assert_eq!(modules.get_requested_modules(d_id), Some(vec![]));
}

#[test]
fn main_and_side_module() {
  let main_specifier = resolve_url("file:///main_module.js").unwrap();
  let side_specifier = resolve_url("file:///side_module.js").unwrap();

  let loader = StaticModuleLoader::new([
    (
      main_specifier.clone(),
      ascii_str!("if (!import.meta.main) throw Error();"),
    ),
    (
      side_specifier.clone(),
      ascii_str!("if (import.meta.main) throw Error();"),
    ),
  ]);

  let mut runtime = JsRuntime::new(RuntimeOptions {
    module_loader: Some(Rc::new(loader)),
    ..Default::default()
  });

  let main_id_fut = runtime
    .load_main_module(&main_specifier, None)
    .boxed_local();
  let main_id = futures::executor::block_on(main_id_fut).unwrap();

  #[allow(clippy::let_underscore_future)]
  let _ = runtime.mod_evaluate(main_id);
  futures::executor::block_on(runtime.run_event_loop(Default::default()))
    .unwrap();

  // Try to add another main module - it should error.
  let side_id_fut = runtime
    .load_main_module(&side_specifier, None)
    .boxed_local();
  futures::executor::block_on(side_id_fut).unwrap_err();

  // And now try to load it as a side module
  let side_id_fut = runtime
    .load_side_module(&side_specifier, None)
    .boxed_local();
  let side_id = futures::executor::block_on(side_id_fut).unwrap();

  #[allow(clippy::let_underscore_future)]
  let _ = runtime.mod_evaluate(side_id);
  futures::executor::block_on(runtime.run_event_loop(Default::default()))
    .unwrap();
}

#[test]
fn dynamic_imports_snapshot() {
  //TODO: Once the issue with the ModuleNamespaceEntryGetter is fixed, we can maintain a reference to the module
  // and use it when loading the snapshot
  let snapshot = {
    const MAIN_WITH_CODE_SRC: FastString = ascii_str!(
      r#"
    await import("./b.js");
  "#
    );

    let loader = MockLoader::new();
    let mut runtime = JsRuntimeForSnapshot::new(RuntimeOptions {
      module_loader: Some(loader),
      ..Default::default()
    });
    // In default resolution code should be empty.
    // Instead we explicitly pass in our own code.
    // The behavior should be very similar to /a.js.
    let spec = resolve_url("file:///main_with_code.js").unwrap();
    let main_id_fut = runtime
      .load_main_module(&spec, Some(MAIN_WITH_CODE_SRC))
      .boxed_local();
    let main_id = futures::executor::block_on(main_id_fut).unwrap();

    #[allow(clippy::let_underscore_future)]
    let _ = runtime.mod_evaluate(main_id);
    futures::executor::block_on(runtime.run_event_loop(Default::default()))
      .unwrap();
    runtime.snapshot()
  };

  let snapshot = Snapshot::JustCreated(snapshot);
  let mut runtime2 = JsRuntime::new(RuntimeOptions {
    startup_snapshot: Some(snapshot),
    ..Default::default()
  });

  //Evaluate the snapshot with an empty function
  runtime2.execute_script_static("check.js", "true").unwrap();
}

#[test]
fn import_meta_snapshot() {
  let snapshot = {
    const MAIN_WITH_CODE_SRC: ModuleCode = ascii_str!(
      r#"
  if (import.meta.url != 'file:///main_with_code.js') throw Error();
  globalThis.meta = import.meta;
  globalThis.url = import.meta.url;
  "#
    );

    let loader = MockLoader::new();
    let mut runtime = JsRuntimeForSnapshot::new(RuntimeOptions {
      module_loader: Some(loader),
      ..Default::default()
    });
    // In default resolution code should be empty.
    // Instead we explicitly pass in our own code.
    // The behavior should be very similar to /a.js.
    let spec = resolve_url("file:///main_with_code.js").unwrap();
    let main_id_fut = runtime
      .load_main_module(&spec, Some(MAIN_WITH_CODE_SRC))
      .boxed_local();
    let main_id = futures::executor::block_on(main_id_fut).unwrap();

    #[allow(clippy::let_underscore_future)]
    let eval_fut = runtime.mod_evaluate(main_id);
    futures::executor::block_on(runtime.run_event_loop(Default::default()))
      .unwrap();
    futures::executor::block_on(eval_fut).unwrap();
    runtime.snapshot()
  };

  let snapshot = Snapshot::JustCreated(snapshot);
  let mut runtime2 = JsRuntime::new(RuntimeOptions {
    startup_snapshot: Some(snapshot),
    ..Default::default()
  });

  runtime2
    .execute_script_static(
      "check.js",
      "if (globalThis.url !== 'file:///main_with_code.js') throw Error('x')",
    )
    .unwrap();
}

#[tokio::test]
async fn no_duplicate_loads() {
  // Both of these imports will cause redirects - ie. their "found" specifier
  // will be different than requested specifier.
  const MAIN_SRC: &str = r#"
  import "https://example.com/foo.js";
  import "https://example.com/bar.js";
  "#;

  // foo.js is importing bar.js that is already redirected
  const FOO_SRC: &str = r#"
  import "https://example.com/v1/bar.js";
  "#;

  struct ConsumingLoader {
    pub files: Rc<RefCell<HashMap<String, Option<String>>>>,
  }

  impl Default for ConsumingLoader {
    fn default() -> Self {
      let files = HashMap::from_iter([
        (
          "https://example.com/v1/foo.js".to_string(),
          Some(FOO_SRC.to_string()),
        ),
        (
          "https://example.com/v1/bar.js".to_string(),
          Some("".to_string()),
        ),
        ("file:///main.js".to_string(), Some(MAIN_SRC.to_string())),
      ]);
      Self {
        files: Rc::new(RefCell::new(files)),
      }
    }
  }

  impl ModuleLoader for ConsumingLoader {
    fn resolve(
      &self,
      specifier: &str,
      referrer: &str,
      _kind: ResolutionKind,
    ) -> Result<ModuleSpecifier, Error> {
      let referrer = if referrer == "." {
        "file:///"
      } else {
        referrer
      };

      Ok(resolve_import(specifier, referrer)?)
    }

    fn load(
      &self,
      module_specifier: &ModuleSpecifier,
      _maybe_referrer: Option<&ModuleSpecifier>,
      _is_dyn_import: bool,
    ) -> Pin<Box<ModuleSourceFuture>> {
      let found_specifier =
        if module_specifier.as_str() == "https://example.com/foo.js" {
          Some("https://example.com/v1/foo.js".to_string())
        } else if module_specifier.as_str() == "https://example.com/bar.js" {
          Some("https://example.com/v1/bar.js".to_string())
        } else {
          None
        };

      let mut files = self.files.borrow_mut();
      eprintln!(
        "getting specifier {} {:?}",
        module_specifier.as_str(),
        found_specifier
      );
      let source_code = if let Some(found) = &found_specifier {
        files.get_mut(found).unwrap().take().unwrap()
      } else {
        files
          .get_mut(module_specifier.as_str())
          .unwrap()
          .take()
          .unwrap()
      };
      let module_source = ModuleSource {
        code: ModuleSourceCode::String(ModuleCode::from(source_code)),
        module_type: ModuleType::JavaScript,
        module_url_specified: module_specifier.clone().into(),
        module_url_found: found_specifier.map(|s| s.into()),
      };
      async move { Ok(module_source) }.boxed_local()
    }
  }

  let loader = Rc::new(ConsumingLoader::default());
  let mut runtime = JsRuntime::new(RuntimeOptions {
    module_loader: Some(loader),
    ..Default::default()
  });

  let spec = resolve_url("file:///main.js").unwrap();
  let a_id = runtime.load_main_module(&spec, None).await.unwrap();
  #[allow(clippy::let_underscore_future)]
  let _ = runtime.mod_evaluate(a_id);
  runtime.run_event_loop(Default::default()).await.unwrap();
}

#[tokio::test]
async fn import_meta_resolve_cb() {
  fn import_meta_resolve_cb(
    _loader: &dyn ModuleLoader,
    specifier: String,
    _referrer: String,
  ) -> Result<ModuleSpecifier, Error> {
    if specifier == "foo" {
      return Ok(ModuleSpecifier::parse("foo:bar").unwrap());
    }

    if specifier == "./mod.js" {
      return Ok(ModuleSpecifier::parse("file:///mod.js").unwrap());
    }

    bail!("unexpected")
  }

  let mut runtime = JsRuntime::new(RuntimeOptions {
    import_meta_resolve_callback: Some(Box::new(import_meta_resolve_cb)),
    ..Default::default()
  });

  let spec = ModuleSpecifier::parse("file:///test.js").unwrap();
  let source = r#"
  if (import.meta.resolve("foo") !== "foo:bar") throw new Error("a");
  if (import.meta.resolve("./mod.js") !== "file:///mod.js") throw new Error("b");
  let caught = false;
  try {
    import.meta.resolve("boom!");
  } catch (e) {
    if (!(e instanceof TypeError)) throw new Error("c");
    caught = true;
  }
  if (!caught) throw new Error("d");
  "#
  .to_string();

  let a_id = runtime
    .load_main_module(&spec, Some(source.into()))
    .await
    .unwrap();
  let local = LocalSet::new();
  let a = local.spawn_local(runtime.mod_evaluate(a_id));
  let b = local.spawn_local(async move {
    runtime.run_event_loop(Default::default()).await
  });
  local.await;

  a.await.unwrap().unwrap();
  b.await.unwrap().unwrap();
}

#[test]
fn builtin_core_module() {
  let main_specifier = resolve_url("ext:///main_module.js").unwrap();

  let source_code =
    r#"import { core, primordials, internals } from "ext:core/mod.js";
if (typeof core === "undefined") throw new Error("core missing");
if (typeof primordials === "undefined") throw new Error("core missing");
if (typeof internals === "undefined") throw new Error("core missing");
"#
    .to_string();
  let loader =
    StaticModuleLoader::new([(main_specifier.clone(), source_code.into())]);

  let mut runtime = JsRuntime::new(RuntimeOptions {
    module_loader: Some(Rc::new(loader)),
    ..Default::default()
  });

  let main_id_fut = runtime
    .load_main_module(&main_specifier, None)
    .boxed_local();
  let main_id = futures::executor::block_on(main_id_fut).unwrap();

  #[allow(clippy::let_underscore_future)]
  let _ = runtime.mod_evaluate(main_id);
  futures::executor::block_on(runtime.run_event_loop(Default::default()))
    .unwrap();
}
