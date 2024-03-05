// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

use anyhow::anyhow;
use anyhow::Context;
use clap::builder::Arg;
use clap::builder::Command;
use clap::ArgMatches;
use deno_core::anyhow::Error;
use deno_core::v8;
use deno_core::CustomModuleEvaluationKind;
use deno_core::FastString;
use deno_core::FsModuleLoader;
use deno_core::JsRuntime;
use deno_core::ModuleSourceCode;
use deno_core::RuntimeOptions;
use std::borrow::Cow;
use std::net::SocketAddr;
use std::rc::Rc;
use std::sync::Arc;

mod inspector_server;
use crate::inspector_server::InspectorServer;

// Uncomment to generate a snapshot
macro_rules! generate_symbols {
  ($num:expr) => {
    static SOURCE_CODE_$num: &str =
      include_str!(concat!("./snapshot/", stringify!($num), ".js"));
    static ONEBYTE_CONST_$num: v8::OneByteConst =
      deno_core::FastStaticString::create_external_onebyte_const(
        SOURCE_CODE_$num.as_bytes(),
      );
    static FAST_STRING_$num: deno_core::FastStaticString =
      deno_core::FastStaticString::new(&ONEBYTE_CONST_$num);
  };
}

// Uncomment to generate a snapshot
macro_rules! generate_symbols_up_to {
  ($num:expr) => {
      $(generate_symbols!($num);)+

      static SOURCE_CODE_MAIN: &str = include_str!("./snapshot/main.js");
      static ONEBYTE_CONST_MAIN: v8::OneByteConst =
          deno_core::FastStaticString::create_external_onebyte_const(SOURCE_CODE_MAIN.as_bytes());
      static FAST_STRING_MAIN: deno_core::FastStaticString =
          deno_core::FastStaticString::new(&ONEBYTE_CONST_MAIN);

      static ESM_FILES: &[deno_core::ExtensionFileSource] = &[
          deno_core::ExtensionFileSource::new(
              "ext:testing_snapshotting/main.js",
              &FAST_STRING_MAIN,
          ),
          $(deno_core::ExtensionFileSource::new(
              concat!("ext:testing_snapshotting/", stringify!($num), ".js"),
              &FAST_STRING_$num,
          ),)+
      ];
  };
}

// Uncomment to generate a snapshot
generate_symbols_up_to!(50);

// Uncomment to use a snapshot
// static SNAPSHOT_BYTES: &[u8] = include_bytes!("../snapshot.bin");

fn main() -> Result<(), Error> {
  eprintln!(
    "🛑 deno_core binary is meant for development and testing purposes."
  );

  let cli = build_cli();
  let mut matches = cli.get_matches();

  let file_path = matches.remove_one::<String>("file_path").unwrap();
  println!("Run {file_path}");

  let (maybe_inspector_addr, maybe_inspect_mode) =
    inspect_arg_parse(&mut matches).unzip();
  let inspector_server = if maybe_inspector_addr.is_some() {
    // TODO(bartlomieju): make it configurable
    let host = "127.0.0.1:9229".parse::<SocketAddr>().unwrap();
    Some(Arc::new(InspectorServer::new(host, "dcore")?))
  } else {
    None
  };

  // Uncomment to generate a snapshot
  let ext = deno_core::Extension {
    name: "testing_snapshotting",
    deps: &[],
    js_files: Cow::Borrowed(&[]),
    esm_files: Cow::Borrowed(ESM_FILES),
    lazy_loaded_esm_files: Cow::Borrowed(&[]),
    esm_entry_point: Some("ext:testing_snapshotting/main.js"),
    ops: Cow::Borrowed(&[]),
    external_references: Cow::Borrowed(&[]),
    global_template_middleware: None,
    global_object_middleware: None,
    op_state_fn: None,
    middleware_fn: None,
    enabled: true,
  };
  let output = deno_core::snapshot::create_snapshot(
    deno_core::snapshot::CreateSnapshotOptions {
      extensions: vec![ext],
      cargo_manifest_dir: env!("CARGO_MANIFEST_DIR"),
      startup_snapshot: None,
      with_runtime_cb: None,
      skip_op_registration: false,
      extension_transpiler: None,
    },
    None,
  )
  .unwrap();
  std::fs::write("./snapshot.bin", output.output).unwrap();
  return Ok(());

  let mut js_runtime = JsRuntime::new(RuntimeOptions {
    // TODO(bartlomieju): Uncomment to run with snapshot
    // startup_snapshot: Some(deno_core::Snapshot::Static(SNAPSHOT_BYTES)),
    module_loader: Some(Rc::new(FsModuleLoader)),
    custom_module_evaluation_cb: Some(Box::new(custom_module_evaluation_cb)),
    inspector: inspector_server.is_some(),
    ..Default::default()
  });

  let runtime = tokio::runtime::Builder::new_current_thread()
    .enable_all()
    .build()?;

  let main_module: deno_core::url::Url = deno_core::resolve_path(
    &file_path,
    &std::env::current_dir().context("Unable to get CWD")?,
  )?;

  if let Some(inspector_server) = inspector_server.clone() {
    inspector_server.register_inspector(
      main_module.to_string(),
      &mut js_runtime,
      matches!(maybe_inspect_mode.unwrap(), InspectMode::WaitForConnection),
    );
  }

  let future = async move {
    let mod_id = js_runtime.load_main_es_module(&main_module).await?;
    let result = js_runtime.mod_evaluate(mod_id);
    js_runtime.run_event_loop(Default::default()).await?;
    result.await
  };
  runtime.block_on(future)
}

fn build_cli() -> Command {
  Command::new("dcore")
    .arg(
      Arg::new("inspect")
        .long("inspect")
        .value_name("HOST_AND_PORT")
        .conflicts_with_all(["inspect-brk", "inspect-wait"])
        .help("Activate inspector on host:port (default: 127.0.0.1:9229)")
        .num_args(0..=1)
        .require_equals(true)
        .value_parser(clap::value_parser!(SocketAddr)),
    )
    .arg(
      Arg::new("inspect-brk")
        .long("inspect-brk")
        .conflicts_with_all(["inspect", "inspect-wait"])
        .value_name("HOST_AND_PORT")
        .help(
          "Activate inspector on host:port, wait for debugger to connect and break at the start of user script",
        )
        .num_args(0..=1)
        .require_equals(true)
        .value_parser(clap::value_parser!(SocketAddr)),
    )
    .arg(
      Arg::new("inspect-wait")
        .long("inspect-wait")
        .conflicts_with_all(["inspect", "inspect-brk"])
        .value_name("HOST_AND_PORT")
        .help(
          "Activate inspector on host:port and wait for debugger to connect before running user code",
        )
        .num_args(0..=1)
        .require_equals(true)
        .value_parser(clap::value_parser!(SocketAddr)),
    )
    .arg(
      Arg::new("file_path")
        .help("A relative or absolute file to a file to run")
        .value_hint(clap::ValueHint::FilePath)
        .value_parser(clap::value_parser!(String))
        .required(true),
    )
}

enum InspectMode {
  Immediate,
  WaitForConnection,
}

fn inspect_arg_parse(
  matches: &mut ArgMatches,
) -> Option<(SocketAddr, InspectMode)> {
  let default = || "127.0.0.1:9229".parse::<SocketAddr>().unwrap();
  if matches.contains_id("inspect") {
    let addr = matches
      .remove_one::<SocketAddr>("inspect")
      .unwrap_or_else(default);
    return Some((addr, InspectMode::Immediate));
  }
  if matches.contains_id("inspect-wait") {
    let addr = matches
      .remove_one::<SocketAddr>("inspect-wait")
      .unwrap_or_else(default);
    return Some((addr, InspectMode::WaitForConnection));
  }

  None
}

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
