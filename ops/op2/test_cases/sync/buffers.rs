// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();

use deno_core::JsBuffer;

#[op2(fast)]
fn op_buffers(#[buffer] _a: &[u8], #[buffer] _b: &mut [u8]) {
}

#[op2(fast)]
fn op_buffers_32(#[buffer] _a: &[u32], #[buffer] _b: &mut [u32]) {
}

#[op2]
fn op_buffers_option(#[buffer] _a: Option<&[u8]>, #[buffer] _b: Option<JsBuffer>) {
}
