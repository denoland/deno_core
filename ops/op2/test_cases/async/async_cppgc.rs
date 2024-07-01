// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();
use deno_core::GarbageCollected;

struct Wrap;

impl GarbageCollected for Wrap {}

#[op2(async)]
#[cppgc]
async fn op_make_cppgc_object() -> Wrap {
  Wrap
}

#[op2(async)]
async fn op_use_cppgc_object(#[cppgc] _wrap: &Wrap) {}

#[op2(async)]
async fn op_use_optional_cppgc_object(#[cppgc] _wrap: Option<&Wrap>) {}
