// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.

use anyhow::anyhow;
use anyhow::Context;
use deno_core::anyhow::Error;
use deno_core::CustomModuleEvaluationKind;
use deno_core::FastString;
use deno_core::FsModuleLoader;
use deno_core::JsRuntime;
use deno_core::ModuleSourceCode;
use deno_core::RuntimeOptions;
use std::borrow::Cow;
use std::rc::Rc;

fn custom_module_evaluation_cb(
  scope: &mut v8::HandleScope,
  module_type: Cow<'_, str>,
  module_name: &FastString,
  code: ModuleSourceCode,
) -> Result<CustomModuleEvaluationKind, Error> {
  match &*module_type {
    "bytes" => bytes_module(scope, code),
    "text" => text_module(scope, module_name, code),
    _ => Err(anyhow!(
      "Can't import {:?} because of unknown module type {}",
      module_name,
      module_type
    )),
  }
}

fn bytes_module(
  scope: &mut v8::HandleScope,
  code: ModuleSourceCode,
) -> Result<CustomModuleEvaluationKind, Error> {
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
) -> Result<CustomModuleEvaluationKind, Error> {
  // FsModuleLoader always returns bytes.
  let ModuleSourceCode::Bytes(buf) = code else {
    unreachable!()
  };

  let code = std::str::from_utf8(buf.as_bytes()).with_context(|| {
    format!("Can't convert {:?} source code to string", module_name)
  })?;
  let str_ = v8::String::new(scope, code).unwrap();
  let value: v8::Local<v8::Value> = str_.into();
  Ok(CustomModuleEvaluationKind::Synthetic(v8::Global::new(
    scope, value,
  )))
}

// TODO(bartlomieju): figure out how we can incorporate snapshotting here
static SNAPSHOT_BYTES: &[u8] = include_bytes!("../snapshot.bin");

// static SOURCE_CODE: &str = include_str!("./testing_snapshotting.js");
// #[allow(long_running_const_eval)]
// static ONEBYTE_CONST: v8::OneByteConst =
//   v8::String::create_external_onebyte_const(SOURCE_CODE.as_bytes());
// static ESM_FILES: &[deno_core::ExtensionFileSource] =
//   &[deno_core::ExtensionFileSource::external_ref_backed(
//     "ext:testing_snapshotting/main.js",
//     SOURCE_CODE,
//     &ONEBYTE_CONST,
//   )];

fn main() -> Result<(), Error> {
  let args: Vec<String> = std::env::args().collect();
  eprintln!(
    "🛑 deno_core binary is meant for development and testing purposes."
  );
  if args.len() < 2 {
    println!("Usage: cargo run -- <path_to_module>");
    std::process::exit(1);
  }
  let main_url = &args[1];
  println!("Run {main_url}");

  // TODO(bartlomieju): figure out how we can incorporate snapshotting here

  // let ext = deno_core::Extension {
  //   name: "testing_snapshotting",
  //   deps: &[],
  //   js_files: Cow::Borrowed(&[]),
  //   esm_files: Cow::Borrowed(ESM_FILES),
  //   lazy_loaded_esm_files: Cow::Borrowed(&[]),
  //   esm_entry_point: Some("ext:testing_snapshotting/main.js"),
  //   ops: Cow::Borrowed(&[]),
  //   external_references: Cow::Borrowed(&[]),
  //   global_template_middleware: None,
  //   global_object_middleware: None,
  //   op_state_fn: None,
  //   middleware_fn: None,
  //   enabled: true,
  // };
  // let output = deno_core::snapshot::create_snapshot(
  //   deno_core::snapshot::CreateSnapshotOptions {
  //     extensions: vec![ext],
  //     cargo_manifest_dir: env!("CARGO_MANIFEST_DIR"),
  //     startup_snapshot: None,
  //     with_runtime_cb: None,
  //     skip_op_registration: false,
  //   },
  //   None,
  // )
  // .unwrap();
  // std::fs::write("./snapshot.bin", output.output).unwrap();
  // return Ok(());

  let mut js_runtime = JsRuntime::new(RuntimeOptions {
    // TODO(bartlomieju): figure out how we can incorporate snapshotting here
    startup_snapshot: Some(SNAPSHOT_BYTES),
    module_loader: Some(Rc::new(FsModuleLoader)),
    custom_module_evaluation_cb: Some(Box::new(custom_module_evaluation_cb)),
    ..Default::default()
  });

  let runtime = tokio::runtime::Builder::new_current_thread()
    .enable_all()
    .build()?;

  let main_module = deno_core::resolve_path(
    main_url,
    &std::env::current_dir().context("Unable to get CWD")?,
  )?;

  let future = async move {
    let mod_id = js_runtime.load_main_module(&main_module, None).await?;
    let result = js_runtime.mod_evaluate(mod_id);
    js_runtime.run_event_loop(Default::default()).await?;
    result.await
  };
  runtime.block_on(future)
}
