// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use deno_core::op2;
use deno_core::v8;

use super::testing::Output;
use super::testing::TestFunctions;

#[op2(fast)]
pub fn op_log_debug(#[string] s: &str) {
  println!("{s}");
}

#[op2(fast)]
pub fn op_log_info(#[state] output: &mut Output, #[string] s: String) {
  println!("{s}");
  output.lines.push(s);
}

#[op2]
pub fn op_test_register(
  #[state] tests: &mut TestFunctions,
  #[string] name: String,
  #[global] f: v8::Global<v8::Function>,
) {
  tests.functions.push((name, f));
}
