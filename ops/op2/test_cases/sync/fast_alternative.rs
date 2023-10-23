// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();

#[op2(fast(op_fast))]
fn op_slow(a: u32, b: u32) -> u32 {
  a + b
}

#[op2(fast)]
fn op_fast(a: u32, b: u32) -> u32 {
  a + b
}

pub trait T {}

#[op2(fast(op_fast_generic::<T>))]
fn op_slow_generic<T: Trait>(a: u32, b: u32) -> u32 {
  a + b
}

#[op2(fast)]
fn op_fast_generic<T: Trait>(a: u32, b: u32) -> u32 {
  a + b
}
