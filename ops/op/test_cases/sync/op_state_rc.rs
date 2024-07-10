// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();

use deno_core::OpState;
use std::cell::RefCell;
use std::rc::Rc;

#[op(fast)]
fn op_state_rc(_state: Rc<RefCell<OpState>>) {}
