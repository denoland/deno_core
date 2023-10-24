// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();

use deno_core::v8;
use deno_core::WasmMemory;
use deno_core::ResourceId;

#[op2(fast)]
fn op_wasm_memory(state: &mut OpState, #[smi] rid: ResourceId, #[memory] mem: WasmMemory) {
    let _s = mem.get(state, rid).expect("invalid wasm memory");
    unimplemented!()
}
