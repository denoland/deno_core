// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();

pub type ResourceId = i16;

#[op2(fast)]
fn op_add(#[smi] id: ResourceId, extra: u16) -> u32 {
    id as u32 + extra as u32
}

pub type StubId = i32;

#[op2(fast)]
#[smi]
fn op_subtract(#[smi] id: StubId, extra: i32) -> StubId {
    id - extra
}
