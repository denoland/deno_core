// Copyright 2018-2025 the Deno authors. MIT license.

#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();

#[op2(async, stack_trace)]
pub async fn op_async_stack_trace() {}
