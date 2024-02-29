// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();

#[op2(fast)]
#[bigint]
pub fn op_bigint() -> u64 {
  0
}
