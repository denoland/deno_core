// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();

struct Wrap;

#[op2]
#[cppgc]
fn op_make_cppgc_object() -> Wrap {
  Wrap
}

#[op2(fast)]
fn op_use_cppgc_object(#[cppgc] _wrap: &Wrap) {}

#[op2(fast)]
fn op_use_cppgc_object_mut(#[cppgc] _wrap: &mut Wrap) {}
