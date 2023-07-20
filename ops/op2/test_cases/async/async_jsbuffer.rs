// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();

#[op2(async)]
#[buffer]
pub async fn op_async_v8_buffer(#[buffer] buf: JsBuffer) -> JsBuffer {
    buf
}
