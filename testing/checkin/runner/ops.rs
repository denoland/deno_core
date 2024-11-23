// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
use std::cell::RefCell;
use std::rc::Rc;

use deno_core::error::JsNativeError;
use deno_core::op2;
use deno_core::stats::RuntimeActivityDiff;
use deno_core::stats::RuntimeActivitySnapshot;
use deno_core::stats::RuntimeActivityStats;
use deno_core::stats::RuntimeActivityStatsFactory;
use deno_core::stats::RuntimeActivityStatsFilter;
use deno_core::v8;
use deno_core::GarbageCollected;
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

pub struct DOMPoint {
  pub x: f64,
  pub y: f64,
  pub z: f64,
  pub w: f64,
}

impl GarbageCollected for DOMPoint {}

#[op2]
impl DOMPoint {
  #[constructor]
  #[cppgc]
  fn new(
    x: Option<f64>,
    y: Option<f64>,
    z: Option<f64>,
    w: Option<f64>,
  ) -> DOMPoint {
    DOMPoint {
      x: x.unwrap_or(0.0),
      y: y.unwrap_or(0.0),
      z: z.unwrap_or(0.0),
      w: w.unwrap_or(0.0),
    }
  }

  #[required(1)]
  #[static_method]
  #[cppgc]
  fn from_point(
    scope: &mut v8::HandleScope,
    other: v8::Local<v8::Object>,
  ) -> Result<DOMPoint, JsNativeError> {
    fn get(
      scope: &mut v8::HandleScope,
      other: v8::Local<v8::Object>,
      key: &str,
    ) -> Option<f64> {
      let key = v8::String::new(scope, key).unwrap();
      other
        .get(scope, key.into())
        .map(|x| x.to_number(scope).unwrap().value())
    }

    Ok(DOMPoint {
      x: get(scope, other, "x").unwrap_or(0.0),
      y: get(scope, other, "y").unwrap_or(0.0),
      z: get(scope, other, "z").unwrap_or(0.0),
      w: get(scope, other, "w").unwrap_or(0.0),
    })
  }

  #[getter]
  fn x(&self) -> f64 {
    self.x
  }

  #[setter]
  fn x(&self, _: f64) {}

  #[getter]
  fn y(&self) -> f64 {
    self.y
  }
  #[getter]
  fn w(&self) -> f64 {
    self.w
  }
  #[getter]
  fn z(&self) -> f64 {
    self.z
  }
}

#[op2(fast)]
pub fn op_nop_generic<T: SomeType + 'static>(state: &mut OpState) {
  state.take::<T>();
}
