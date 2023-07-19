// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();

struct Something {}

#[op2(fast)]
fn op_state_rc(#[state] _arg: &Something, #[state] _arg_opt: Option<&Something>) {}
