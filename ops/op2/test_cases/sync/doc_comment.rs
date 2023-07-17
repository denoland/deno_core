// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();

/// This is a doc comment.
#[op2(fast)]
pub fn op_has_doc_comment() -> () {}
