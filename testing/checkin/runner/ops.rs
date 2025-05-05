// Copyright 2018-2025 the Deno authors. MIT license.

use std::cell::Cell;
use std::cell::RefCell;
use std::rc::Rc;

use deno_core::GarbageCollected;
use deno_core::OpState;
use deno_core::op2;
use deno_core::stats::RuntimeActivityDiff;
use deno_core::stats::RuntimeActivitySnapshot;
use deno_core::stats::RuntimeActivityStats;
use deno_core::stats::RuntimeActivityStatsFactory;
use deno_core::stats::RuntimeActivityStatsFilter;
use deno_core::v8;
use deno_error::JsErrorBox;

use super::Output;
use super::TestData;
use super::extensions::SomeType;

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

impl GarbageCollected for TestObjectWrap {
  fn get_name(&self) -> &'static std::ffi::CStr {
    c"TestObjectWrap"
  }
}

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
  fn with_scope_fast(&self, _scope: &mut v8::HandleScope) {}

  #[fast]
  #[rename("with_RENAME")]
  fn with_rename(&self) {}

  #[async_method]
  async fn with_async_fn(&self, #[smi] ms: u32) -> Result<(), JsErrorBox> {
    tokio::time::sleep(std::time::Duration::from_millis(ms as u64)).await;
    Ok(())
  }

  #[fast]
  fn with_this(&self, #[this] _: v8::Global<v8::Object>) {}

  #[getter]
  #[string]
  fn with_slow_getter(&self) -> String {
    String::from("getter")
  }
}

pub struct DOMPoint {}

impl GarbageCollected for DOMPoint {
  fn get_name(&self) -> &'static std::ffi::CStr {
    c"DOMPoint"
  }
}

impl DOMPointReadOnly {
  fn from_point_inner(
    scope: &mut v8::HandleScope,
    other: v8::Local<v8::Object>,
  ) -> Result<DOMPointReadOnly, JsErrorBox> {
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

    Ok(DOMPointReadOnly {
      x: get(scope, other, "x").unwrap_or(0.0).into(),
      y: get(scope, other, "y").unwrap_or(0.0).into(),
      z: get(scope, other, "z").unwrap_or(0.0).into(),
      w: get(scope, other, "w").unwrap_or(0.0).into(),
    })
  }
}

pub struct DOMPointReadOnly {
  x: Cell<f64>,
  y: Cell<f64>,
  z: Cell<f64>,
  w: Cell<f64>,
}

impl GarbageCollected for DOMPointReadOnly {
  fn get_name(&self) -> &'static std::ffi::CStr {
    c"DOMPointReadOnly"
  }
}

#[op2(base)]
impl DOMPointReadOnly {
  #[getter]
  fn x(&self) -> f64 {
    self.x.get()
  }

  #[getter]
  fn y(&self) -> f64 {
    self.y.get()
  }

  #[getter]
  fn z(&self) -> f64 {
    self.z.get()
  }

  #[getter]
  fn w(&self) -> f64 {
    self.w.get()
  }
}

#[op2(inherit = DOMPointReadOnly)]
impl DOMPoint {
  #[constructor]
  #[cppgc]
  fn new(
    x: Option<f64>,
    y: Option<f64>,
    z: Option<f64>,
    w: Option<f64>,
  ) -> (DOMPointReadOnly, DOMPoint) {
    let ro = DOMPointReadOnly {
      x: Cell::new(x.unwrap_or(0.0)),
      y: Cell::new(y.unwrap_or(0.0)),
      z: Cell::new(z.unwrap_or(0.0)),
      w: Cell::new(w.unwrap_or(0.0)),
    };

    (ro, DOMPoint {})
  }

  #[required(1)]
  #[static_method]
  #[cppgc]
  fn from_point(
    scope: &mut v8::HandleScope,
    other: v8::Local<v8::Object>,
  ) -> Result<DOMPointReadOnly, JsErrorBox> {
    DOMPointReadOnly::from_point_inner(scope, other)
  }

  #[required(1)]
  #[cppgc]
  fn from_point(
    &self,
    scope: &mut v8::HandleScope,
    other: v8::Local<v8::Object>,
  ) -> Result<DOMPointReadOnly, JsErrorBox> {
    DOMPointReadOnly::from_point_inner(scope, other)
  }

  #[setter]
  fn x(&self, x: f64, #[proto] ro: &DOMPointReadOnly) {
    ro.x.set(x);
  }

  #[getter]
  fn x(&self, #[proto] ro: &DOMPointReadOnly) -> f64 {
    ro.x.get()
  }

  #[setter]
  fn y(&self, y: f64, #[proto] ro: &DOMPointReadOnly) {
    ro.y.set(y);
  }

  #[getter]
  fn y(&self, #[proto] ro: &DOMPointReadOnly) -> f64 {
    ro.y.get()
  }

  #[setter]
  fn z(&self, z: f64, #[proto] ro: &DOMPointReadOnly) {
    ro.z.set(z);
  }

  #[getter]
  fn z(&self, #[proto] ro: &DOMPointReadOnly) -> f64 {
    ro.z.get()
  }

  #[setter]
  fn w(&self, w: f64, #[proto] ro: &DOMPointReadOnly) {
    ro.w.set(w);
  }

  #[getter]
  fn w(&self, #[proto] ro: &DOMPointReadOnly) -> f64 {
    ro.w.get()
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

impl GarbageCollected for TestEnumWrap {
  fn get_name(&self) -> &'static std::ffi::CStr {
    c"TestEnumWrap"
  }
}

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

pub struct Foo;

impl deno_core::GarbageCollected for Foo {
  fn get_name(&self) -> &'static std::ffi::CStr {
    c"Foo"
  }
}

#[op2(fast)]
pub fn op_scope_cppgc(_scope: &mut v8::HandleScope, #[cppgc] _foo: &Foo) {}
