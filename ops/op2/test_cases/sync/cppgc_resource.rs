// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();

#[op2(fast)]
fn op_file(#[cppgc] _file: &std::fs::File) {}

struct Wrap;

#[op2]
#[cppgc]
fn op_make_cppgc_object() -> Wrap {
    Wrap
}
