// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
deno_ops_compile_test_runner::prelude!();

pub type ResourceId = i16;

#[op2(fast)]
fn op_add(#[smi] id: ResourceId, extra: u16) -> u32 {
    0
}
