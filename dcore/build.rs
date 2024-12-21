// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

use std::env;
use std::fs;
use std::path::Path;

fn main() {
  let out_dir = env::var_os("OUT_DIR").unwrap();
  let snapshot = deno_core_testing::create_snapshot();
  let dest_path = Path::new(&out_dir).join("SNAPSHOT.bin");
  fs::write(dest_path, snapshot).expect("Failed to write snapshot");
}
