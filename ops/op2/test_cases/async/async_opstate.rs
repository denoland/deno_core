// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();

use deno_core::OpState;
use std::cell::RefCell;
use std::rc::Rc;

#[op2(async)]
pub async fn op_async_opstate(
  state: Rc<RefCell<OpState>>,
) -> std::io::Result<i32> {
  Ok(*state.borrow().borrow::<i32>())
}
