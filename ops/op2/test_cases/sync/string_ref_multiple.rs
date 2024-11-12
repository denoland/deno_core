// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();

#[op2(fast)]
fn op_string_ref_multiple(#[string] s: &str, #[string] s2: &str) -> u32 {
  s.len() as _
}
