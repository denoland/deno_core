// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();

#[op(fast)]
fn op_string_owned(#[string] s: &str) -> u32 {
  s.len() as _
}
