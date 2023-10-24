// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.

use deno_core::include_ascii_string;
use deno_core::op2;
use deno_core::Extension;
use deno_core::JsRuntime;
use deno_core::Op;
use deno_core::OpState;
use deno_core::RuntimeOptions;
use deno_core::WasmMemory;

#[op2(fast)]
fn op_wasm(state: &mut OpState, #[smi] rid: u32, #[memory] memory: WasmMemory) {
  let memory = memory.get(state, rid).expect("invalid wasm memory");
  memory[0] = 69;
}

fn main() {
  // Build a deno_core::Extension providing custom ops
  let ext = Extension {
    name: "my_ext",
    ops: std::borrow::Cow::Borrowed(&[op_wasm::DECL]),
    ..Default::default()
  };

  // Initialize a runtime instance
  let mut runtime = JsRuntime::new(RuntimeOptions {
    extensions: vec![ext],
    ..Default::default()
  });

  runtime
    .execute_script("<usage>", include_ascii_string!("wasm.js"))
    .unwrap();
}
