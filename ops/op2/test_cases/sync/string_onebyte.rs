// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();

use std::borrow::Cow;

#[op2(fast)]
fn op_string_onebyte(#[string(onebyte)] s: Cow<[u8]>) -> u32 {
    s.len() as _
}
