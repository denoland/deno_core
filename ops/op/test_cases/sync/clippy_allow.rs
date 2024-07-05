// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();

/// This is a doc comment.
#[op(fast)]
#[allow(clippy::some_annotation)]
pub fn op_extra_annotation() -> () {}

#[op(fast)]
pub fn op_clippy_internal() -> () {
  {
    #![allow(clippy::await_holding_refcell_ref)]
  }
}
