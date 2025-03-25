// Copyright 2018-2025 the Deno authors. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();
use deno_core::cppgc::PrototypeChain;
use deno_core::GarbageCollected;

struct Wrap;

impl GarbageCollected for Wrap {}

impl PrototypeChain for Wrap {}

#[op2(fast)]
fn op_use_cppgc_object(#[cppgc] _wrap: &'static Wrap) {}

#[op2(fast)]
fn op_use_buffer(#[buffer] _buffer: &'static [u8]) {}
