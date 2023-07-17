// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
deno_ops_compile_test_runner::prelude!();

use deno_core::v8;

#[op2]
pub fn op_v8_lifetime<'s>(s: v8::Local<'s, v8::String>) -> v8::Local<'s, v8::String> {
    unimplemented!()
}
