// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

use deno_core::error::AnyError;
use deno_core::op2;

#[op2]
#[string]
pub fn op_fs_read_text_file(
  #[string] file_path: &str,
) -> Result<String, AnyError> {
  let content = std::fs::read_to_string(file_path)?;
  Ok(content)
}

#[op2(fast)]
pub fn op_fs_write_text_file(
  #[string] file_path: &str,
  #[string] content: &str,
) -> Result<(), AnyError> {
  std::fs::write(file_path, content)?;
  Ok(())
}
