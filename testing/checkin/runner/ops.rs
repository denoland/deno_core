// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
use std::cell::RefCell;
use std::rc::Rc;

use deno_core::op2;
use deno_core::stats::RuntimeActivityDiff;
use deno_core::stats::RuntimeActivitySnapshot;
use deno_core::stats::RuntimeActivityStats;
use deno_core::stats::RuntimeActivityStatsFactory;
use deno_core::stats::RuntimeActivityStatsFilter;
use deno_core::v8;
use deno_core::OpState;

use super::testing::Output;
use super::testing::TestData;
use super::testing::TestFunctions;

#[op2(fast)]
pub fn op_log_debug(#[string] s: &str) {
  println!("{s}");
}

#[op2(fast)]
pub fn op_log_info(#[state] output: &mut Output, #[string] s: String) {
  println!("{s}");
  output.line(s);
}

#[op2]
pub fn op_test_register(
  #[state] tests: &mut TestFunctions,
  #[string] name: String,
  #[global] f: v8::Global<v8::Function>,
) {
  tests.functions.push((name, f));
}

#[op2(fast)]
pub fn op_stats_capture(#[string] name: String, state: Rc<RefCell<OpState>>) {
  let stats = state
    .borrow()
    .borrow::<RuntimeActivityStatsFactory>()
    .clone();
  let data = stats.capture(&RuntimeActivityStatsFilter::all());
  let mut state = state.borrow_mut();
  let test_data = state.borrow_mut::<TestData>();
  test_data.insert(name, data);
}

#[op2]
#[serde]
pub fn op_stats_dump(
  #[string] name: String,
  #[state] test_data: &mut TestData,
) -> RuntimeActivitySnapshot {
  let stats = test_data.get::<RuntimeActivityStats>(name);
  stats.dump()
}

#[op2]
#[serde]
pub fn op_stats_diff(
  #[string] before: String,
  #[string] after: String,
  #[state] test_data: &mut TestData,
) -> RuntimeActivityDiff {
  let before = test_data.get::<RuntimeActivityStats>(before);
  let after = test_data.get::<RuntimeActivityStats>(after);
  RuntimeActivityStats::diff(before, after)
}

#[op2(fast)]
pub fn op_stats_delete(
  #[string] name: String,
  #[state] test_data: &mut TestData,
) {
  test_data.take::<RuntimeActivityStats>(name);
}

struct Stateful {
  name: String,
}

impl Stateful {
  #[op2(method(Stateful))]
  #[string]
  fn get_name(&self) -> String {
    self.name.clone()
  }

  #[op2(fast, method(Stateful))]
  fn print_name(&self) {
    println!("{}", self.name);
  }

  #[op2(async, method(Stateful))]
  #[string]
  async fn async_method(&self) -> String {
    self.name.clone()
  }
}
