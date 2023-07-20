// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();

use deno_core::v8;

// Collect a few examples that we'll smoke test when not running on the CI.

#[op2(fast)]
pub fn op_fast(x: u32) -> u32 {
    x
}

#[op2(fast)]
fn op_buffers(#[buffer] _a: &[u8], #[buffer(copy)] _b: Vec<u8>) {
}

struct Something {}

#[op2(fast)]
fn op_state_rc(#[state] _arg: &Something, #[state] _arg_opt: Option<&Something>) {}

#[op2(fast)]
fn op_v8_1(_s: v8::Local<v8::String>) {}

#[op2(fast)]
fn op_v8_2(_s: &v8::String) {}

#[op2]
fn op_v8_3(#[global] _s: v8::Global<v8::String>) {}
