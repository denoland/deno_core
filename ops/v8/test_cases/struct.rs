// Copyright 2018-2025 the Deno authors. MIT license.

#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();

#[derive(FromV8, ToV8)]
pub struct MyStruct {
  a: deno_core::convert::Smi<u8>,
  r#b: String,
  #[v8(rename = "e")]
  d: deno_core::convert::Smi<u32>,
}
