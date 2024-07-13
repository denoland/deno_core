// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
use deno_core::{
  extension,
  snapshot::{create_snapshot, CreateSnapshotOptions},
};
use std::path::PathBuf;
use std::{env, fs};

fn main() {
  extension!(
    runjs_extension,
    // Must specify an entrypoint so that our module gets loaded while snapshotting:
    esm_entry_point = "my:runtime",
    esm = [
      dir "src",
      "my:runtime" = "runtime.js",
    ],
  );

  let options = CreateSnapshotOptions {
    cargo_manifest_dir: env!("CARGO_MANIFEST_DIR"),
    startup_snapshot: None,
    extensions: vec![runjs_extension::init_ops_and_esm()],
    with_runtime_cb: None,
    skip_op_registration: false,
    extension_transpiler: None,
  };
  let warmup_script = None;

  let snapshot =
    create_snapshot(options, warmup_script).expect("Error creating snapshot");

  // Save the snapshot for use by our source code:
  let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());
  let file_path = out_dir.join("RUNJS_SNAPSHOT.bin");
  fs::write(file_path, snapshot.output).expect("Failed to write snapshot");

  // Let cargo know that builds depend on these files:
  for path in snapshot.files_loaded_during_snapshot {
    println!("cargo:rerun-if-changed={}", path.display());
  }
}
