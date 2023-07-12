// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.

#[op2(fast)]
pub fn op_v8_lifetime<'s>(s: Option<&v8::String>, s2: Option<&v8::String>) {}

