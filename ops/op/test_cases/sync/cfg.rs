// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();

/// This is a doc comment.
#[op(fast)]
#[cfg(windows)]
pub fn op_maybe_windows() -> () {}

/// This is a doc comment.
#[op(fast)]
#[cfg(not(windows))]
pub fn op_maybe_windows() -> () {}
