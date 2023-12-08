// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use anyhow::bail;
use anyhow::Error;
use deno_core::url::Url;
use deno_core::CrossIsolateStore;
use deno_core::JsRuntime;
use deno_core::PollEventLoopOptions;
use deno_core::RuntimeOptions;
use pretty_assertions::assert_eq;
use std::path::Path;
use std::path::PathBuf;
use std::rc::Rc;
use testing::Output;
use tokio::fs::File;
use tokio::io::AsyncReadExt;

use self::testing::TestFunctions;

mod ops;
mod testing;
mod ts_module_loader;

deno_core::extension!(
  checkin_runtime,
  ops = [
    ops::op_log_debug,
    ops::op_log_info,
    ops::op_test_register,
  ],
  esm_entry_point = "ext:checkin_runtime/__init.js",
  esm = [
    dir "checkin/runtime",
    "__init.js",
    "console.ts" with_specifier "checkin:console",
    "testing.ts" with_specifier "checkin:testing",
  ],
  state = |state| {
    state.put(TestFunctions::default());
    state.put(Output::default());
  }
);

fn create_runtime() -> JsRuntime {
  let mut extensions = vec![checkin_runtime::init_ops_and_esm()];

  for extension in &mut extensions {
    use ts_module_loader::maybe_transpile_source;
    for source in extension.esm_files.to_mut() {
      maybe_transpile_source(source).unwrap();
    }
    for source in extension.js_files.to_mut() {
      maybe_transpile_source(source).unwrap();
    }
  }

  JsRuntime::new(RuntimeOptions {
    extensions,
    module_loader: Some(Rc::new(
      ts_module_loader::TypescriptModuleLoader::default(),
    )),
    get_error_class_fn: Some(&|error| {
      deno_core::error::get_custom_error_class(error).unwrap()
    }),
    shared_array_buffer_store: Some(CrossIsolateStore::default()),
    ..Default::default()
  })
}

/// Run a integration test within the `checkin` runtime. This executes a single file, imports and all,
/// and compares its output with the `.out` file in the same directory.
pub fn run_integration_test(test: &str) {
  let runtime = create_runtime();
  let tokio = tokio::runtime::Builder::new_current_thread()
    .enable_all()
    .build()
    .expect("Failed to build a runtime");
  tokio
    .block_on(run_integration_test_task(runtime, test.to_owned()))
    .expect("Failed to complete test");
}

async fn run_integration_test_task(
  mut runtime: JsRuntime,
  test: String,
) -> Result<(), Error> {
  let test_dir = get_test_dir(&["integration", &test]);
  let url = get_test_url(&test_dir, &test)?;
  let module = runtime.load_main_module(&url, None).await?;
  let f = runtime.mod_evaluate(module);
  let mut actual_output = String::new();
  if let Err(e) = runtime
    .run_event_loop(PollEventLoopOptions {
      pump_v8_message_loop: true,
      wait_for_inspector: false,
    })
    .await
  {
    for line in e.to_string().split('\n') {
      actual_output += "[ERR] ";
      actual_output += line;
      actual_output += "\n";
    }
  }
  f.await?;
  let mut output: Output = runtime.op_state().borrow_mut().take();
  output.lines.push(String::new());
  let mut expected_output = String::new();
  File::open(test_dir.join(format!("{test}.out")))
    .await?
    .read_to_string(&mut expected_output)
    .await?;
  actual_output += &output.lines.join("\n");
  assert_eq!(actual_output, expected_output);
  Ok(())
}

/// Run a unit test within the `checkin` runtime. This loads a file which registers a number of tests,
/// then each test is run individually and failures are printed.
pub fn run_unit_test(test: &str) {
  let runtime = create_runtime();
  let tokio = tokio::runtime::Builder::new_current_thread()
    .enable_all()
    .build()
    .expect("Failed to build a runtime");
  tokio
    .block_on(run_unit_test_task(runtime, test.to_owned()))
    .expect("Failed to complete test");
}

async fn run_unit_test_task(
  mut runtime: JsRuntime,
  test: String,
) -> Result<(), Error> {
  let test_dir = get_test_dir(&["unit"]);
  let url = get_test_url(&test_dir, &test)?;
  let module = runtime.load_main_module(&url, None).await?;
  let f = runtime.mod_evaluate(module);
  runtime
    .run_event_loop(PollEventLoopOptions {
      pump_v8_message_loop: true,
      wait_for_inspector: false,
    })
    .await?;
  f.await?;

  let tests: TestFunctions = runtime.op_state().borrow_mut().take();
  for (name, function) in tests.functions {
    println!("Testing {name}...");
    runtime.call_and_await(&function).await?;
    runtime
      .run_event_loop(PollEventLoopOptions {
        pump_v8_message_loop: true,
        wait_for_inspector: false,
      })
      .await?;
  }

  Ok(())
}

fn get_test_dir(dirs: &[&str]) -> PathBuf {
  let mut test_dir = Path::new(env!("CARGO_MANIFEST_DIR")).to_owned();
  for dir in dirs {
    test_dir = test_dir.join(dir).to_owned();
  }

  test_dir.to_owned()
}

fn get_test_url(test_dir: &Path, test: &str) -> Result<Url, Error> {
  let mut path = None;
  for extension in ["ts", "js", "nocompile"] {
    let test_path = test_dir.join(format!("{test}.{extension}"));
    if test_path.exists() {
      path = Some(test_path);
      break;
    }
  }
  let Some(path) = path else {
    bail!("Test file not found");
  };
  let path = path.canonicalize()?.to_owned();
  let url = Url::from_file_path(path).unwrap().to_string();
  let base_url = Url::from_file_path(Path::new(env!("CARGO_MANIFEST_DIR")))
    .unwrap()
    .to_string();
  let url = Url::parse(&format!("test://{}", &url[base_url.len()..]))?;
  Ok(url)
}
