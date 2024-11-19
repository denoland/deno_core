// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();

#[op(fast)]
pub fn op_bool(arg: bool) -> bool {
  arg
}
