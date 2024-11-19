// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();

use deno_core::v8;

// Unused scope would normally make this a slow-only op
#[op(fast(op_fast))]
fn op_slow(_scope: &v8::HandleScope, a: u32, b: u32) -> u32 {
  a + b
}

#[op(fast)]
fn op_fast(a: u32, b: u32) -> u32 {
  a + b
}

pub trait Trait {}

// Unused scope would normally make this a slow-only op
#[op(fast(op_fast_generic::<T>))]
fn op_slow_generic<T: Trait>(_scope: &v8::HandleScope, a: u32, b: u32) -> u32 {
  a + b
}

#[op(fast)]
fn op_fast_generic<T: Trait>(a: u32, b: u32) -> u32 {
  a + b
}
