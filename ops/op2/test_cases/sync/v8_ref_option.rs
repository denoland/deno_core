// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.

#[op2]
pub fn op_v8_lifetime<'s>(s: Option<&v8::String>, s2: Option<&mut v8::String>) {}

