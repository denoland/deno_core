// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use deno_core::v8;

#[derive(Default)]
pub struct Output {
  pub lines: Vec<String>,
}

#[derive(Default)]
pub struct TestFunctions {
  pub functions: Vec<(String, v8::Global<v8::Function>)>,
}
