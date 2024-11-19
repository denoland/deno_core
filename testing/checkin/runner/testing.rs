// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
use anyhow::bail;
use anyhow::Error;
use deno_core::op2;
use deno_core::url::Url;
use deno_core::v8;
use deno_core::JsRuntime;
use deno_core::PollEventLoopOptions;
use pretty_assertions::assert_eq;
use std::path::Path;
use std::path::PathBuf;
use tokio::fs::File;
use tokio::io::AsyncReadExt;

use super::create_runtime;
use super::run_async;
use super::Output;
use super::TestData;

deno_core::extension!(
  checkin_testing,
  ops = [
    op_test_register,
  ],
  esm_entry_point = "checkin:testing",
  esm = [
    dir "checkin/runtime",
    "checkin:testing" = "testing.ts",
  ],
  state = |state| {
    state.put(TestFunctions::default());
  }
);

#[derive(Default)]
pub struct TestFunctions {
  pub functions: Vec<(String, v8::Global<v8::Function>)>,
}

#[op2]
pub fn op_test_register(
  #[state] tests: &mut TestFunctions,
  #[string] name: String,
  #[global] f: v8::Global<v8::Function>,
) {
  tests.functions.push((name, f));
}

/// Run a integration test within the `checkin` runtime. This executes a single file, imports and all,
/// and compares its output with the `.out` file in the same directory.
pub fn run_integration_test(test: &str) {
  let (runtime, _) =
    create_runtime(None, vec![checkin_testing::init_ops_and_esm()]);
  run_async(run_integration_test_task(runtime, test.to_owned()));
}

async fn run_integration_test_task(
  mut runtime: JsRuntime,
  test: String,
) -> Result<(), Error> {
  let test_dir = get_test_dir(&["integration", &test]);
  let url = get_test_url(&test_dir, &test)?;
  let module = runtime.load_main_es_module(&url).await?;
  let f = runtime.mod_evaluate(module);
  let mut actual_output = String::new();
  if let Err(e) = runtime
    .run_event_loop(PollEventLoopOptions::default())
    .await
  {
    let state = runtime.op_state().clone();
    let state = state.borrow();
    let output: &Output = state.borrow();
    for line in e.to_string().split('\n') {
      output.line(format!("[ERR] {line}"));
    }
  } else {
    // Only await the module if we didn't fail
    if let Err(e) = f.await {
      let state = runtime.op_state().clone();
      let state = state.borrow();
      let output: &Output = state.borrow();
      for line in e.to_string().split('\n') {
        output.line(format!("[ERR] {line}"));
      }
    }
  }
  let mut lines = runtime.op_state().borrow_mut().take::<Output>().take();
  lines.push(String::new());
  let mut expected_output = String::new();
  File::open(test_dir.join(format!("{test}.out")))
    .await?
    .read_to_string(&mut expected_output)
    .await?;
  actual_output += &lines.join("\n");
  assert_eq!(actual_output, expected_output);
  Ok(())
}

/// Run a unit test within the `checkin` runtime. This loads a file which registers a number of tests,
/// then each test is run individually and failures are printed.
pub fn run_unit_test(test: &str) {
  let (runtime, _) =
    create_runtime(None, vec![checkin_testing::init_ops_and_esm()]);
  run_async(run_unit_test_task(runtime, test.to_owned()));
}

async fn run_unit_test_task(
  mut runtime: JsRuntime,
  test: String,
) -> Result<(), Error> {
  let test_dir = get_test_dir(&["unit"]);
  let url = get_test_url(&test_dir, &test)?;
  let module = runtime.load_main_es_module(&url).await?;
  let f = runtime.mod_evaluate(module);
  runtime
    .run_event_loop(PollEventLoopOptions::default())
    .await?;
  f.await?;

  let tests: TestFunctions = runtime.op_state().borrow_mut().take();
  for (name, function) in tests.functions {
    println!("Testing {name}...");
    let call = runtime.call(&function);
    runtime
      .with_event_loop_promise(call, PollEventLoopOptions::default())
      .await?;

    // Clear any remaining test data so we have a fresh state
    let state = runtime.op_state();
    let mut state = state.borrow_mut();
    let data = state.borrow_mut::<TestData>();
    data.data.clear();
  }

  Ok(())
}

fn get_test_dir(dirs: &[&str]) -> PathBuf {
  let mut test_dir = Path::new(env!("CARGO_MANIFEST_DIR")).to_owned();
  for dir in dirs {
    test_dir.join(dir).clone_into(&mut test_dir);
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
