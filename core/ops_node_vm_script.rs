// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

use crate::error::AnyError;
use crate::op2;
use anyhow::bail;
use serde::Deserialize;
use v8::MapFnTo;

struct ContextifyScript {
  script: v8::Global<v8::UnboundScript>,
}

impl Drop for ContextifyScript {
  fn drop(&mut self) {
    // TODO
  }
}

impl ContextifyScript {
  fn new() {}
  fn instance_of() {}
  fn create_cached_data() {}
  fn run_in_context() {}
  fn eval_machine() {}
}
