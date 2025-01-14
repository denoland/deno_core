// Copyright 2018-2025 the Deno authors. MIT license.

use std::cell::RefCell;
use std::rc::Rc;

use deno_core::op2;
use deno_core::stats::RuntimeActivityDiff;
use deno_core::stats::RuntimeActivitySnapshot;
use deno_core::stats::RuntimeActivityStats;
use deno_core::stats::RuntimeActivityStatsFactory;
use deno_core::stats::RuntimeActivityStatsFilter;
use deno_core::v8;
use deno_core::GarbageCollected;
use deno_core::OpState;
use deno_error::JsErrorBox;

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

pub struct TestObjectWrap {}

impl GarbageCollected for TestObjectWrap {}

#[op2]
impl TestObjectWrap {
  #[constructor]
  #[cppgc]
  fn new(_: bool) -> TestObjectWrap {
    TestObjectWrap {}
  }

  #[fast]
  #[smi]
  fn with_varargs(
    &self,
    #[varargs] args: Option<&v8::FunctionCallbackArguments>,
  ) -> u32 {
    args.map(|args| args.length() as u32).unwrap_or(0)
  }

  #[fast]
  #[rename("with_RENAME")]
  fn with_rename(&self) {}

  #[async_method]
  async fn with_async_fn(&self, #[smi] ms: u32) -> Result<(), JsErrorBox> {
    tokio::time::sleep(std::time::Duration::from_millis(ms as u64)).await;
    Ok(())
  }

  #[getter]
  #[string]
  fn with_slow_getter(&self) -> String {
    String::from("getter")
  }
}

pub struct DOMPoint {
  pub x: f64,
  pub y: f64,
  pub z: f64,
  pub w: f64,
}

impl GarbageCollected for DOMPoint {}

impl DOMPoint {
  fn from_point_inner(
    scope: &mut v8::HandleScope,
    other: v8::Local<v8::Object>,
  ) -> Result<DOMPoint, JsErrorBox> {
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
}

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
  ) -> Result<DOMPoint, JsErrorBox> {
    DOMPoint::from_point_inner(scope, other)
  }

  #[required(1)]
  #[cppgc]
  fn from_point(
    &self,
    scope: &mut v8::HandleScope,
    other: v8::Local<v8::Object>,
  ) -> Result<DOMPoint, JsErrorBox> {
    DOMPoint::from_point_inner(scope, other)
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

  #[fast]
  #[getter]
  fn z(&self) -> f64 {
    self.z
  }

  #[fast]
  fn wrapping_smi(&self, #[smi] t: u32) -> u32 {
    t
  }

  #[fast]
  #[symbol("symbolMethod")]
  fn with_symbol(&self) {}

  #[fast]
  #[stack_trace]
  fn with_stack_trace(&self) {}
}

#[repr(u8)]
#[derive(Clone, Copy)]
pub enum TestEnumWrap {
  #[allow(dead_code)]
  A,
}

impl GarbageCollected for TestEnumWrap {}

#[op2]
impl TestEnumWrap {
  #[getter]
  fn as_int(&self) -> u8 {
    *self as u8
  }
}

#[op2(fast)]
pub fn op_nop_generic<T: SomeType + 'static>(state: &mut OpState) {
  state.take::<T>();
}
