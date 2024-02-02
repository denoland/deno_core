// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use anyhow::bail;
use anyhow::Error;
use deno_core::url::Url;
use deno_core::CrossIsolateStore;
use deno_core::JsRuntime;
use deno_core::PollEventLoopOptions;
use deno_core::RuntimeOptions;
use futures::Future;
use pretty_assertions::assert_eq;
use std::path::Path;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc::channel;
use std::sync::mpsc::RecvTimeoutError;
use std::time::Duration;
use testing::Output;
use tokio::fs::File;
use tokio::io::AsyncReadExt;

use crate::checkin::runner::testing::TestData;

use self::ops_worker::worker_create;
use self::ops_worker::WorkerCloseWatcher;
use self::ops_worker::WorkerHostSide;
use self::testing::TestFunctions;

mod ops;
mod ops_async;
mod ops_buffer;
mod ops_error;
mod ops_io;
mod ops_worker;
mod testing;
mod ts_module_loader;

deno_core::extension!(
  checkin_runtime,
  ops = [
    ops::op_log_debug,
    ops::op_log_info,
    ops::op_test_register,
    ops::op_stats_capture,
    ops::op_stats_diff,
    ops::op_stats_dump,
    ops::op_stats_delete,
    ops_io::op_pipe_create,
    ops_async::op_async_yield,
    ops_async::op_async_barrier_create,
    ops_async::op_async_barrier_await,
    ops_async::op_async_spin_on_state,
    ops_error::op_async_throw_error_eager,
    ops_error::op_async_throw_error_lazy,
    ops_error::op_async_throw_error_deferred,
    ops_error::op_error_custom_sync,
    ops_error::op_error_context_sync,
    ops_error::op_error_context_async,
    ops_buffer::op_v8slice_store,
    ops_buffer::op_v8slice_clone,
    ops_worker::op_worker_spawn,
    ops_worker::op_worker_send,
    ops_worker::op_worker_recv,
    ops_worker::op_worker_parent,
    ops_worker::op_worker_await_close,
    ops_worker::op_worker_terminate,
  ],
  esm_entry_point = "ext:checkin_runtime/__init.js",
  esm = [
    dir "checkin/runtime",
    "__bootstrap.js",
    "__init.js",
    "async.ts" with_specifier "checkin:async",
    "console.ts" with_specifier "checkin:console",
    "error.ts" with_specifier "checkin:error",
    "testing.ts" with_specifier "checkin:testing",
    "timers.ts" with_specifier "checkin:timers",
    "worker.ts" with_specifier "checkin:worker",
  ],
  state = |state| {
    state.put(TestFunctions::default());
    state.put(TestData::default());
  }
);

fn create_runtime(
  output: Output,
  parent: Option<WorkerCloseWatcher>,
) -> (JsRuntime, WorkerHostSide) {
  let (worker, worker_host_side) = worker_create(parent);
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

  let mut runtime = JsRuntime::new(RuntimeOptions {
    extensions,
    module_loader: Some(Rc::new(
      ts_module_loader::TypescriptModuleLoader::default(),
    )),
    get_error_class_fn: Some(&|error| {
      deno_core::error::get_custom_error_class(error).unwrap_or("Error")
    }),
    shared_array_buffer_store: Some(CrossIsolateStore::default()),
    ..Default::default()
  });
  let stats = runtime.runtime_activity_stats_factory();
  runtime.op_state().borrow_mut().put(stats);
  runtime.op_state().borrow_mut().put(worker);
  runtime.op_state().borrow_mut().put(output);
  (runtime, worker_host_side)
}

fn run_async(f: impl Future<Output = Result<(), Error>>) {
  let tokio = tokio::runtime::Builder::new_current_thread()
    .enable_all()
    .build()
    .expect("Failed to build a runtime");
  tokio.block_on(f).expect("Failed to run the given task");

  // We don't have a good way to wait for tokio to go idle here, but we'd like tokio
  // to poll any remaining tasks to shake out any errors.
  let handle = tokio.spawn(async {
    tokio::task::yield_now().await;
  });
  _ = tokio.block_on(handle);

  let (tx, rx) = channel::<()>();
  let timeout = std::thread::spawn(move || {
    if rx.recv_timeout(Duration::from_secs(10))
      == Err(RecvTimeoutError::Timeout)
    {
      panic!("Failed to shut down the runtime in time");
    }
  });
  drop(tokio);
  drop(tx);
  _ = timeout.join();
}

/// Run a integration test within the `checkin` runtime. This executes a single file, imports and all,
/// and compares its output with the `.out` file in the same directory.
pub fn run_integration_test(test: &str) {
  let (runtime, _) = create_runtime(Output::default(), None);
  run_async(run_integration_test_task(runtime, test.to_owned()));
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
    .run_event_loop(PollEventLoopOptions::default())
    .await
  {
    let state = runtime.op_state().clone();
    let state = state.borrow();
    let output: &Output = state.borrow();
    for line in e.to_string().split('\n') {
      output.line(format!("[ERR] {line}"));
    }
  }
  f.await?;
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
  let (runtime, _) = create_runtime(Output::default(), None);
  run_async(run_unit_test_task(runtime, test.to_owned()));
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
