// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
deno_ops_compile_test_runner::prelude!();

#[op2(fast)]
fn op_buffers(#[buffer] a: &[u8], #[buffer] b: &mut [u8]) {
}
