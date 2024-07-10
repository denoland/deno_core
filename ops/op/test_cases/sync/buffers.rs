// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();

use deno_core::JsBuffer;

#[op(fast)]
fn op_buffers(
  #[buffer] _a: &[u8],
  #[buffer] _b: &mut [u8],
  #[buffer] _c: *const u8,
  #[buffer] _d: *mut u8,
) {
}

#[op(fast)]
fn op_buffers_32(
  #[buffer] _a: &[u32],
  #[buffer] _b: &mut [u32],
  #[buffer] _c: *const u32,
  #[buffer] _d: *mut u32,
) {
}

#[op]
fn op_buffers_option(
  #[buffer] _a: Option<&[u8]>,
  #[buffer] _b: Option<JsBuffer>,
) {
}
