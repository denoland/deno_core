// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();

#[op(nofast)]
fn op_nofast(a: u32, b: u32) -> u32 {
  a + b
}
