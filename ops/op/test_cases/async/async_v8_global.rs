// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();

use deno_core::v8;

#[op(async)]
pub async fn op_async_v8_global(#[global] _s: v8::Global<v8::String>) {}
