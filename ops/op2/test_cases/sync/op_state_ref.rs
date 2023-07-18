// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();

use deno_core::OpState;

#[op2(fast)]
fn op_state_rc(_state: &OpState) {}
