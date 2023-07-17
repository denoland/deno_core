// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();

#[op2]
#[string]
pub fn op_string_return() -> Option<String> {
    Some("".to_owned())
}
