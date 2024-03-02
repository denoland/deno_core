// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

use anyhow::anyhow;
use anyhow::Context;
use clap::builder::Arg;
use clap::builder::Command;
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

fn build_cli() -> Command {
  Command::new("dcore")
    .arg(
      Arg::new("snapshot")
        .long("snapshot")
        .help("Optional path to a snapshot file that will be loaded on startup")
        .value_hint(clap::ValueHint::FilePath)
        .value_parser(clap::value_parser!(String))
        .require_equals(true),
    )
    .arg(
      Arg::new("file_to_run")
        .help("A relative or absolute file to a file to run")
        .value_hint(clap::ValueHint::FilePath)
        .value_parser(clap::value_parser!(String))
        .required(true),
    )
}

fn main() -> Result<(), Error> {
  eprintln!(
    "🛑 deno_core binary is meant for development and testing purposes."
  );

  let cli = build_cli();
  let matches = cli.get_matches();

  let main_url = matches.get_one::<String>("file_to_run").unwrap();
  let load_snapshot = matches.get_one::<String>("snapshot");

  println!("Run {main_url}");

  // TODO(bartlomieju): figure out how we can incorporate snapshotting here
  // deno_core::snapshot::create_snapshot(
  //   CreateSnapshotOptions {
  //     serializer: Box::new(SnapshotFileSerializer::new(
  //       std::fs::File::create("./snapshot.bin").unwrap(),
  //     )),
  //     extensions: vec![],
  //     cargo_manifest_dir: env!("CARGO_MANIFEST_DIR"),
  //     startup_snapshot: None,
  //     with_runtime_cb: None,
  //     skip_op_registration: false,
  //   },
  //   None,
  // )
  // .unwrap();
  // return Ok(());

  let startup_snapshot: Option<&'static [u8]> =
    if let Some(snapshot_path) = load_snapshot {
      let data = std::fs::read(snapshot_path).with_context(|| {
        format!("Failed to load snapshot from: {}", snapshot_path)
      })?;
      let boxed_data = data.into_boxed_slice();
      // Leak so we can obtain a static reference to the slice.
      let static_data = Box::leak(boxed_data);
      Some(static_data)
    } else {
      None
    };

  let mut js_runtime = JsRuntime::new(RuntimeOptions {
    startup_snapshot,
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
    let mod_id = js_runtime.load_main_es_module(&main_module).await?;
    let result = js_runtime.mod_evaluate(mod_id);
    js_runtime.run_event_loop(Default::default()).await?;
    result.await
  };
  runtime.block_on(future)
}
