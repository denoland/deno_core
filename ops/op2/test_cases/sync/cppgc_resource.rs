// Copyright 2018-2025 the Deno authors. MIT license.

#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();
use deno_core::cppgc::PrototypeChain;
use deno_core::GarbageCollected;

struct Wrap;

impl GarbageCollected for Wrap {}

impl PrototypeChain for Wrap {}

#[op2(fast)]
fn op_cppgc_object(#[cppgc] _resource: &Wrap) {}

#[op2(fast)]
fn op_option_cppgc_object(#[cppgc] _resource: Option<&Wrap>) {}

#[op2]
#[cppgc]
fn op_make_cppgc_object() -> Wrap {
  Wrap
}

#[op2(fast)]
fn op_use_cppgc_object(#[cppgc] _wrap: &Wrap) {}

#[op2]
#[cppgc]
fn op_make_cppgc_object_option() -> Option<Wrap> {
  Some(Wrap)
}

#[op2(fast)]
fn op_use_cppgc_object_option(#[cppgc] _wrap: &Wrap) {}
