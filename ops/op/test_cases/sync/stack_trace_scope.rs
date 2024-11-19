// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();

use deno_core::error::JsStackFrame;

#[op(fast)]
fn op_stack_trace(
  #[string] _: String,
  #[stack_trace] _: Option<Vec<JsStackFrame>>,
) {
}
