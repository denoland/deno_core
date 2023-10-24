// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.

// asc wasm.ts --exportStart --initialMemory 6400 -O -o wasm.wasm
// deno-fmt-ignore
const bytes = new Uint8Array([
  0,  97, 115, 109,   1,  0,   0,   0,   1,   8,   2,  96,
  1, 127,   0,  96,   0,  0,   2,  16,   1,   4, 119,  97,
  115, 109,   7, 111, 112, 95, 119,  97, 115, 109,   0,   0,
  3,   3,   2,   0,   1,  5,   4,   1,   0, 128,  50,   7,
  36,   4,   7, 111, 112, 95, 119,  97, 115, 109,   0,   0,
  4,  99,  97, 108, 108,  0,   1,   6, 109, 101, 109, 111,
  114, 121,   2,   0,   6, 95, 115, 116,  97, 114, 116,   0,
  2,  10,  12,   2,   6,  0,  32,   0,  16,   0,  11,   3,
  0,   1,  11,   4,  10,   0,  32,   0,  16,   0,  11,   3,
]);

const { ops } = Deno.core;

const module = new WebAssembly.Module(bytes);
const instance = new WebAssembly.Instance(module, { wasm: ops });
const rid = ops.op_core_set_wasm_memory(instance.exports.memory);

instance.exports.call(rid);

const memory = instance.exports.memory;
const view = new Uint8Array(memory.buffer);

if (view[0] !== 69) {
  throw new Error("Expected first byte to be 69");
}
