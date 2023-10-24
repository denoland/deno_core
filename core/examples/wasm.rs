// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
#![allow(deprecated)]
use deno_core::*;
use std::mem::transmute;
use std::ptr::NonNull;

struct WasmMemory(NonNull<v8::WasmMemoryObject>);

fn wasm_memory_unchecked(state: &mut OpState) -> &mut [u8] {
  let WasmMemory(global) = state.borrow::<WasmMemory>();
  // SAFETY: `v8::Local` is always non-null pointer; the `HandleScope` is
  // already on the stack, but we don't have access to it.
  let memory_object = unsafe {
    transmute::<NonNull<v8::WasmMemoryObject>, v8::Local<v8::WasmMemoryObject>>(
      *global,
    )
  };
  let backing_store = memory_object.buffer().get_backing_store();
  let ptr = backing_store.data().unwrap().as_ptr() as *mut u8;
  let len = backing_store.byte_length();
  // SAFETY: `ptr` is a valid pointer to `len` bytes.
  unsafe { std::slice::from_raw_parts_mut(ptr, len) }
}

#[op(wasm)]
fn op_wasm(state: &mut OpState, memory: Option<&mut [u8]>) {
  let memory = memory.unwrap_or_else(|| wasm_memory_unchecked(state));
  memory[0] = 69;
}

#[op(v8)]
fn op_set_wasm_mem(
  scope: &mut v8::HandleScope,
  state: &mut OpState,
  memory: serde_v8::Value,
) {
  let memory =
    v8::Local::<v8::WasmMemoryObject>::try_from(memory.v8_value).unwrap();
  let global = v8::Global::new(scope, memory);
  state.put(WasmMemory(global.into_raw()));
}

fn main() {
  // Build a deno_core::Extension providing custom ops
  let ext = Extension {
    name: "my_ext",
    ops: std::borrow::Cow::Borrowed(&[op_wasm::DECL, op_set_wasm_mem::DECL]),
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
