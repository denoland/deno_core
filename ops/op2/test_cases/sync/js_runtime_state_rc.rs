// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();

use deno_core::JsRuntimeState;
use std::cell::RefCell;
use std::rc::Rc;

#[op2(fast)]
fn js_runtime_state_rc(_state: Rc<RefCell<JsRuntimeState>>) {}
