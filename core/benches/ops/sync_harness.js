// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
// deno-lint-ignore-file no-unused-vars, prefer-const

// This harness is dynamically generated for each individual bench run.
const LARGE_STRING_1000000 = "*".repeat(1000000);
const LARGE_STRING_1000 = "*".repeat(1000);
const LARGE_STRING_UTF8_1000000 = "\u1000".repeat(1000000);
const LARGE_STRING_UTF8_1000 = "\u1000".repeat(1000);
const BUFFER = new Uint8Array(1024);
const ARRAYBUFFER = new ArrayBuffer(1024);
const { __OP__: op } = Deno.core.ensureFastOps();
const { op_make_external } = Deno.core.ensureFastOps();
const EXTERNAL = op_make_external();

// TODO(mmastrac): Because of current v8 limitations, these ops are not always fast unless we do this.
// The reason is not entirely clear.
function __OP__(__ARGS__) {
  return op(__ARGS__);
}

let accum = __INIT__;
let __index__ = 0;
__PERCENT__PrepareFunctionForOptimization(__OP__);
__CALL__;
__PERCENT__OptimizeFunctionOnNextCall(__OP__);
__CALL__;

function bench() {
  let accum = __INIT__;
  for (let __index__ = 0; __index__ < __COUNT__; __index__++) __CALL__;
  return accum;
}
__PERCENT__PrepareFunctionForOptimization(bench);
bench();
__PERCENT__OptimizeFunctionOnNextCall(bench);
bench();
