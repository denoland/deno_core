// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
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

#[op2]
#[buffer]
fn op_get_wasm_module() -> Vec<u8> {
  include_bytes!("wasm.wasm").as_slice().to_vec()
}

#[op2(fast)]
fn op_wasm(state: &mut OpState, #[memory(caller)] memory: Option<&mut [u8]>) {
  let memory = memory.unwrap_or_else(|| wasm_memory_unchecked(state));
  memory[0] = 69;
}

#[op2(fast)]
fn op_wasm_mem(memory: &v8::WasmMemoryObject) {
  // let memory = memory.unwrap_or_else(|| wasm_memory_unchecked(state));
  // memory[0] = 69;
  let len = memory.buffer().byte_length();
  let slice = unsafe {
    std::ptr::slice_from_raw_parts_mut(
      memory.buffer().data().unwrap().as_ptr() as *mut u8,
      len,
    )
    .as_mut()
    .unwrap()
  };
  slice[0] = 68;
}

#[op2]
fn op_set_wasm_mem(
  state: &mut OpState,
  #[global] memory: v8::Global<v8::WasmMemoryObject>,
) {
  state.put(WasmMemory(memory.into_raw()));
}

// Build a deno_core::Extension providing custom ops
deno_core::extension!(
  wasm_example,
  ops = [op_get_wasm_module, op_wasm, op_wasm_mem, op_set_wasm_mem]
);

fn main() {
  // Initialize a runtime instance
  let (mut runtime, _) = JsRuntime::new(RuntimeOptions {
    extensions: vec![wasm_example::init_ops()],
    ..Default::default()
  });

  runtime
    .execute_script("<usage>", ascii_str_include!("wasm.js"))
    .unwrap();
}
