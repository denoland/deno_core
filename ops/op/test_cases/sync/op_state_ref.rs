// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();

use deno_core::v8;
use deno_core::OpState;

// Test w/ import pollution
#[allow(unused)]
use std::borrow::Borrow;
#[allow(unused)]
use std::borrow::BorrowMut;

#[op(fast)]
fn op_state_ref(_state: &OpState) {}

#[op(fast)]
fn op_state_mut(_state: &mut OpState) {}

#[op]
fn op_state_and_v8(
  _state: &mut OpState,
  #[global] _callback: v8::Global<v8::Function>,
) {
}

#[op(fast)]
fn op_state_and_v8_local(
  _state: &mut OpState,
  _callback: v8::Local<v8::Function>,
) {
}
