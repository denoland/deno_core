// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
use self::ops_worker::worker_create;
use self::ops_worker::WorkerCloseWatcher;
use self::ops_worker::WorkerHostSide;
use self::ts_module_loader::maybe_transpile_source;
use anyhow::Context;
use deno_core::error::JsNativeError;
use deno_core::v8;
use deno_core::CrossIsolateStore;
use deno_core::CustomModuleEvaluationKind;
use deno_core::Extension;
use deno_core::FastString;
use deno_core::ImportAssertionsSupport;
use deno_core::JsRuntime;
use deno_core::ModuleSourceCode;
use deno_core::RuntimeOptions;
use futures::Future;
use std::any::Any;
use std::any::TypeId;
use std::borrow::Cow;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::mpsc::channel;
use std::sync::mpsc::RecvTimeoutError;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

mod extensions;
mod ops;
mod ops_async;
mod ops_buffer;
mod ops_error;
mod ops_io;
mod ops_worker;
pub mod snapshot;
#[cfg(test)]
pub mod testing;
mod ts_module_loader;

#[derive(Clone, Default)]
pub struct Output {
  pub lines: Arc<Mutex<Vec<String>>>,
}
impl Output {
  pub fn line(&self, line: String) {
    self.lines.lock().unwrap().push(line)
  }

  #[cfg(test)]
  pub fn take(&self) -> Vec<String> {
    std::mem::take(&mut self.lines.lock().unwrap())
  }
}

#[derive(Default)]
pub struct TestData {
  pub data: HashMap<(String, TypeId), Box<dyn Any>>,
}

impl TestData {
  pub fn insert<T: 'static + Any>(&mut self, name: String, data: T) {
    self.data.insert((name, TypeId::of::<T>()), Box::new(data));
  }

  pub fn get<T: 'static + Any>(&self, name: String) -> &T {
    let key = (name, TypeId::of::<T>());
    self
      .data
      .get(&key)
      .unwrap_or_else(|| {
        panic!(
          "Unable to locate '{}' of type {}",
          key.0,
          std::any::type_name::<T>()
        )
      })
      .downcast_ref()
      .unwrap()
  }

  pub fn take<T: 'static + Any>(&mut self, name: String) -> T {
    let key = (name, TypeId::of::<T>());
    let Some(res) = self.data.remove(&key) else {
      panic!(
        "Failed to remove '{}' of type {}",
        key.0,
        std::any::type_name::<T>()
      );
    };
    *res.downcast().unwrap()
  }
}

pub fn create_runtime(
  parent: Option<WorkerCloseWatcher>,
  additional_extensions: Vec<Extension>,
) -> (JsRuntime, WorkerHostSide) {
  let (worker, worker_host_side) = worker_create(parent);
  let snapshot = snapshot::create_snapshot();
  let snapshot = Box::leak(snapshot);
  let mut runtime =
    create_runtime_from_snapshot(snapshot, false, additional_extensions);
  runtime.op_state().borrow_mut().put(worker);
  (runtime, worker_host_side)
}

pub fn create_runtime_from_snapshot(
  snapshot: &'static [u8],
  inspector: bool,
  additional_extensions: Vec<Extension>,
) -> JsRuntime {
  let mut extensions = vec![extensions::checkin_runtime::init_ops::<()>()];
  extensions.extend(additional_extensions);
  let module_loader =
    Rc::new(ts_module_loader::TypescriptModuleLoader::default());
  let mut runtime = JsRuntime::new(RuntimeOptions {
    extensions,
    startup_snapshot: Some(snapshot),
    module_loader: Some(module_loader.clone()),
    extension_transpiler: Some(Rc::new(|specifier, source| {
      maybe_transpile_source(specifier, source)
    })),
    shared_array_buffer_store: Some(CrossIsolateStore::default()),
    custom_module_evaluation_cb: Some(Box::new(custom_module_evaluation_cb)),
    inspector,
    import_assertions_support: ImportAssertionsSupport::Warning,
    ..Default::default()
  });

  let stats = runtime.runtime_activity_stats_factory();
  runtime.op_state().borrow_mut().put(stats);
  runtime.op_state().borrow_mut().put(Output::default());
  runtime
}

fn run_async(f: impl Future<Output = Result<(), anyhow::Error>>) {
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

fn custom_module_evaluation_cb(
  scope: &mut v8::HandleScope,
  module_type: Cow<'_, str>,
  module_name: &FastString,
  code: ModuleSourceCode,
) -> Result<CustomModuleEvaluationKind, JsNativeError> {
  match &*module_type {
    "bytes" => bytes_module(scope, code),
    "text" => text_module(scope, module_name, code),
    _ => Err(JsNativeError::generic(format!(
      "Can't import {:?} because of unknown module type {}",
      module_name, module_type
    ))),
  }
}

fn bytes_module(
  scope: &mut v8::HandleScope,
  code: ModuleSourceCode,
) -> Result<CustomModuleEvaluationKind, JsNativeError> {
  // FsModuleLoader always returns bytes.
  let ModuleSourceCode::Bytes(buf) = code else {
    unreachable!()
  };
  let owned_buf = buf.to_vec();
  let buf_len: usize = owned_buf.len();
  let backing_store = v8::ArrayBuffer::new_backing_store_from_vec(owned_buf);
  let backing_store_shared = backing_store.make_shared();
  let ab = v8::ArrayBuffer::with_backing_store(scope, &backing_store_shared);
  let uint8_array = v8::Uint8Array::new(scope, ab, 0, buf_len).unwrap();
  let value: v8::Local<v8::Value> = uint8_array.into();
  Ok(CustomModuleEvaluationKind::Synthetic(v8::Global::new(
    scope, value,
  )))
}

fn text_module(
  scope: &mut v8::HandleScope,
  module_name: &FastString,
  code: ModuleSourceCode,
) -> Result<CustomModuleEvaluationKind, JsNativeError> {
  // FsModuleLoader always returns bytes.
  let ModuleSourceCode::Bytes(buf) = code else {
    unreachable!()
  };

  let code = std::str::from_utf8(buf.as_bytes())
    .with_context(|| {
      format!("Can't convert {:?} source code to string", module_name)
    })
    .map_err(JsNativeError::from_err)?;
  let str_ = v8::String::new(scope, code).unwrap();
  let value: v8::Local<v8::Value> = str_.into();
  Ok(CustomModuleEvaluationKind::Synthetic(v8::Global::new(
    scope, value,
  )))
}
