// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
deno_ops_compile_test_runner::prelude!();

/// This is a doc comment.
#[op2(fast)]
#[allow(clippy::some_annotation)]
pub fn op_extra_annotation() -> () {}
