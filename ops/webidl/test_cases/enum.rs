// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();

#[derive(WebIDL)]
#[webidl(enum)]
pub enum Enumeration {
  FooBar,
  Baz,
  #[webidl(rename = "hello")]
  World,
}
