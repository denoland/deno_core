// Copyright 2018-2025 the Deno authors. MIT license.

#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();
use deno_core::cppgc::GarbageCollected;
use deno_core::v8;
use std::cell::Cell;

pub struct Foo {
  x: Cell<u32>,
}

impl GarbageCollected for Foo {
  fn get_name(&self) -> &'static std::ffi::CStr {
    c"Foo"
  }
}

#[op2]
impl Foo {
  #[constructor]
  #[cppgc]
  pub fn constructor(x: Option<u32>) -> Foo {
    Foo {
      x: Cell::new(x.unwrap_or_default()),
    }
  }

  #[fast]
  #[getter]
  pub fn x(&self) -> u32 {
    self.x.get()
  }

  #[fast]
  #[setter]
  pub fn x(&self, x: u32) {
    self.x.set(x);
  }

  #[nofast]
  #[required(1)]
  pub fn bar(&self, _v: u32) {}

  #[fast]
  pub fn zzz(&self) {}

  #[nofast]
  fn with_varargs(
    &self,
    #[varargs] _args: Option<&v8::FunctionCallbackArguments>,
  ) {
  }

  #[nofast]
  #[rename("with_RENAME")]
  fn with_rename(&self) {}

  #[nofast]
  #[static_method]
  fn do_thing() {}

  #[nofast]
  fn do_thing(&self) {}
}
