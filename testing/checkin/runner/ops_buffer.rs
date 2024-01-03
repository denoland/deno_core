use deno_core::op2;
use deno_core::JsBuffer;

use super::testing::TestData;

#[op2(fast)]
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
