// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();

#[derive(WebIDL)]
#[webidl(dictionary)]
pub struct Dict {
  a: u8,
  #[options(clamp = true)]
  b: Vec<u16>,
  #[webidl(default = Some(3))]
  c: Option<u32>,
  #[webidl(rename = "e")]
  d: u64,
  f: std::collections::HashMap<String, f32>,
}
