// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();

pub type ResourceId = i16;

#[op2(async)]
#[smi]
pub async fn op_async(#[smi] rid: ResourceId) -> std::io::Result<ResourceId> {
    Ok(rid)
}
