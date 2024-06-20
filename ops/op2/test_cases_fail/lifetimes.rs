// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();
use deno_core::error::AnyError;
use std::future::Future;

struct Wrap;

#[op2(fast)]
fn op_use_cppgc_object(#[cppgc] _wrap: &'static Wrap) {}

#[op2(fast)]
fn op_use_buffer(#[buffer] _buffer: &'static [u8]) {}

#[op2(async)]
async fn op_use_cppgc_object_mut_async(#[cppgc] _wrap: &mut Wrap) {}
