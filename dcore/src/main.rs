// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

use clap::builder::Arg;
use clap::builder::Command;
use clap::ArgMatches;
use deno_core::anyhow::Error;

use deno_core::RuntimeOptions;
use deno_core_testing::create_runtime_from_snapshot;

use std::net::SocketAddr;

use anyhow::Context;
use std::sync::Arc;

static SNAPSHOT: &[u8] =
  include_bytes!(concat!(env!("OUT_DIR"), "/SNAPSHOT.bin"));

mod inspector_server;
mod metrics;
use crate::inspector_server::InspectorServer;
use crate::metrics::create_metrics;

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

  let (metrics_summary, mut js_runtime) = if matches.get_flag("strace-ops")
    || matches.get_flag("strace-ops-summary")
  {
    let (summary, op_metrics_factory_fn) = create_metrics(
      matches.get_flag("strace-ops"),
      matches.get_flag("strace-ops-summary"),
    );
    (
      Some(summary),
      deno_core_testing::create_runtime_from_snapshot_with_options(
        SNAPSHOT,
        inspector_server.is_some(),
        vec![],
        RuntimeOptions {
          op_metrics_factory_fn: Some(op_metrics_factory_fn),
          ..Default::default()
        },
      ),
    )
  } else {
    (
      None,
      create_runtime_from_snapshot(
        SNAPSHOT,
        inspector_server.is_some(),
        vec![],
      ),
    )
  };

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
  let result = runtime.block_on(future);
  if let Some(summary) = metrics_summary {
    eprintln!("{}", summary.to_json_pretty()?)
  }
  result.map_err(|e| e.into())
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
    .arg(
      Arg::new("strace-ops")
        .help("Output a trace of op execution on stderr")
        .long("strace-ops")
        .num_args(0)
        .required(false)
        .action(clap::ArgAction::SetTrue)

    ).arg(
      Arg::new("strace-ops-summary")
        .help("Output a summary of op execution on stderr when program exits")
        .long("strace-ops-summary")
        .action(clap::ArgAction::SetTrue)
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
