// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();

/// This is a doc comment.
#[op2(fast)]
#[allow(clippy::some_annotation)]
pub fn op_extra_annotation() -> () {}

#[op2(fast)]
pub fn op_clippy_internal() -> () {
  {
    #![allow(clippy::await_holding_refcell_ref)]
  }
}
