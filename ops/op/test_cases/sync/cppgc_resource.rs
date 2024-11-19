// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();
use deno_core::GarbageCollected;

struct Wrap;

impl GarbageCollected for Wrap {}

#[op(fast)]
fn op_cppgc_object(#[cppgc] _resource: &Wrap) {}

#[op(fast)]
fn op_option_cppgc_object(#[cppgc] _resource: Option<&Wrap>) {}

#[op]
#[cppgc]
fn op_make_cppgc_object() -> Wrap {
  Wrap
}

#[op(fast)]
fn op_use_cppgc_object(#[cppgc] _wrap: &Wrap) {}

#[op]
#[cppgc]
fn op_make_cppgc_object_option() -> Option<Wrap> {
  Some(Wrap)
}

#[op(fast)]
fn op_use_cppgc_object_option(#[cppgc] _wrap: &Wrap) {}
