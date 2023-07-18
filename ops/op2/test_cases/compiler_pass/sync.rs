// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();

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
