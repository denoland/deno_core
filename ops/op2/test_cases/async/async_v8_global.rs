// Copyright 2018-2025 the Deno authors. MIT license.

#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();

use deno_core::v8;

#[op2(async)]
pub async fn op_async_v8_global(#[global] _s: v8::Global<v8::String>) {}
