// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
use std::cell::RefCell;
use std::rc::Rc;

use deno_core::op2;
use deno_core::stats::RuntimeActivityDiff;
use deno_core::stats::RuntimeActivitySnapshot;
use deno_core::stats::RuntimeActivityStats;
use deno_core::stats::RuntimeActivityStatsFactory;
use deno_core::stats::RuntimeActivityStatsFilter;
use deno_core::GarbageCollected;
use deno_core::OpDecl;
use deno_core::OpState;

use super::extensions::SomeType;
use super::Output;
use super::TestData;

#[op2(fast)]
pub fn op_log_debug(#[string] s: &str) {
  println!("{s}");
}

#[op2(fast)]
pub fn op_log_info(#[state] output: &mut Output, #[string] s: String) {
  println!("{s}");
  output.line(s);
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

#[derive(Debug, Clone)]
pub struct Stateful {
  name: String,
}

impl GarbageCollected for Stateful {}

#[op2]
impl Stateful {
  #[constructor]
  #[cppgc]
  fn new(#[string] name: String) -> Stateful {
    Stateful { name }
  }

  #[method]
  #[cppgc]
  fn get(&self) -> Self {
    self.clone()
  }

  #[method]
  #[string]
  fn get_name(&self) -> String {
    self.name.clone()
  }

  #[fast]
  #[smi]
  fn len(&self) -> u32 {
    self.name.len() as u32
  }
}

struct DOMPoint {
  x: f64,
  y: f64,
  z: f64,
  w: f64,
}

impl GarbageCollected for DOMPoint {}

#[op2]
impl DOMPoint {
  #[constructor]
  #[cppgc]
  fn new(x: f64, y: f64, z: f64, w: f64) -> DOMPoint {
    DOMPoint { x, y, z, w }
  }
}

#[op2(fast)]
pub fn op_nop_generic<T: SomeType + 'static>(state: &mut OpState) {
  state.take::<T>();
}
