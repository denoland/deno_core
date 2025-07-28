// Copyright 2018-2025 the Deno authors. MIT license.

use deno_core::JsBuffer;
use deno_core::op2;

use super::TestData;

#[op2]
pub fn op_v8slice_store(
  #[state] test_data: &mut TestData,
  #[string] name: String,
  #[buffer] data: JsBuffer,
) {
  test_data.insert(name, data);
}

#[op2]
#[buffer]
pub fn op_v8slice_clone(
  #[state] test_data: &mut TestData,
  #[string] name: String,
) -> Vec<u8> {
  test_data.get::<JsBuffer>(name).to_vec()
}
