// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
deno_ops_compile_test_runner::prelude!();

use deno_core::v8;

#[op2]
fn op_v8_string<'a>(str1: &v8::String, str2: v8::Local<v8::String>) -> v8::Local<'a, v8::String> {
    unimplemented!()
}
