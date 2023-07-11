// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.

#[op2(fast)]
fn op_state_rc(#[state] arg: &Something, #[state] arg_opt: Option<&Something>) {}
