// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
use std::rc::Rc;

use deno_core::JsRuntimeForSnapshot;
use deno_core::RuntimeOptions;

use super::extensions;
use super::ts_module_loader::maybe_transpile_source;

pub fn create_snapshot() -> Box<[u8]> {
  let extensions_for_snapshot =
    vec![extensions::checkin_runtime::init_ops_and_esm::<()>()];

  let runtime_for_snapshot = JsRuntimeForSnapshot::new(RuntimeOptions {
    extensions: extensions_for_snapshot,
    extension_transpiler: Some(Rc::new(|specifier, source| {
      maybe_transpile_source(specifier, source)
    })),
    ..Default::default()
  });

  runtime_for_snapshot.snapshot()
}
