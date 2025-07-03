// Copyright 2018-2025 the Deno authors. MIT license.

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
use deno_core::v8::cppgc::GcCell;
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

unsafe impl GarbageCollected for TestObjectWrap {
  fn get_name(&self) -> &'static std::ffi::CStr {
    c"TestObjectWrap"
  }

  fn trace(&self, _visitor: &mut v8::cppgc::Visitor) {}
}

fn int(
  _scope: &mut v8::HandleScope,
  value: v8::Local<v8::Value>,
) -> Result<(), JsErrorBox> {
  if value.is_int32() {
    return Ok(());
  }

  Err(JsErrorBox::type_error("Expected int"))
}

fn int_op(
  _scope: &mut v8::HandleScope,
  args: &v8::FunctionCallbackArguments,
) -> Result<(), JsErrorBox> {
  if args.length() != 1 {
    return Err(JsErrorBox::type_error("Expected one argument"));
  }

  Ok(())
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
  #[undefined]
  fn undefined_result(&self) -> Result<(), JsErrorBox> {
    Ok(())
  }

  #[fast]
  #[rename("with_RENAME")]
  fn with_rename(&self) {}

  #[async_method]
  async fn with_async_fn(&self, #[smi] ms: u32) -> Result<(), JsErrorBox> {
    tokio::time::sleep(std::time::Duration::from_millis(ms as u64)).await;
    Ok(())
  }

  #[fast]
  #[validate(int_op)]
  fn with_validate_int(
    &self,
    #[validate(int)]
    #[smi]
    t: u32,
  ) -> Result<u32, JsErrorBox> {
    Ok(t)
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

unsafe impl GarbageCollected for DOMPoint {
  fn get_name(&self) -> &'static std::ffi::CStr {
    c"DOMPoint"
  }

  fn trace(&self, _visitor: &mut v8::cppgc::Visitor) {}
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
      x: GcCell::new(get(scope, other, "x").unwrap_or(0.0)),
      y: GcCell::new(get(scope, other, "y").unwrap_or(0.0)),
      z: GcCell::new(get(scope, other, "z").unwrap_or(0.0)),
      w: GcCell::new(get(scope, other, "w").unwrap_or(0.0)),
    })
  }
}

pub struct DOMPointReadOnly {
  x: GcCell<f64>,
  y: GcCell<f64>,
  z: GcCell<f64>,
  w: GcCell<f64>,
}

unsafe impl GarbageCollected for DOMPointReadOnly {
  fn get_name(&self) -> &'static std::ffi::CStr {
    c"DOMPointReadOnly"
  }

  fn trace(&self, _visitor: &mut v8::cppgc::Visitor) {}
}

#[op2(base)]
impl DOMPointReadOnly {
  #[getter]
  fn x(&self, scope: &mut v8::HandleScope) -> f64 {
    *self.x.get(scope)
  }

  #[getter]
  fn y(&self, scope: &mut v8::HandleScope) -> f64 {
    *self.y.get(scope)
  }

  #[getter]
  fn z(&self, scope: &mut v8::HandleScope) -> f64 {
    *self.z.get(scope)
  }

  #[getter]
  fn w(&self, scope: &mut v8::HandleScope) -> f64 {
    *self.w.get(scope)
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
      x: GcCell::new(x.unwrap_or(0.0)),
      y: GcCell::new(y.unwrap_or(0.0)),
      z: GcCell::new(z.unwrap_or(0.0)),
      w: GcCell::new(w.unwrap_or(0.0)),
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
  fn x(
    &self,
    scope: &mut v8::HandleScope,
    x: f64,
    #[proto] ro: &DOMPointReadOnly,
  ) {
    ro.x.set(scope, x);
  }

  #[getter]
  fn x(
    &self,
    scope: &mut v8::HandleScope,
    #[proto] ro: &DOMPointReadOnly,
  ) -> f64 {
    *ro.x.get(scope)
  }

  #[setter]
  fn y(
    &self,
    scope: &mut v8::HandleScope,
    y: f64,
    #[proto] ro: &DOMPointReadOnly,
  ) {
    ro.y.set(scope, y);
  }

  #[getter]
  fn y(
    &self,
    scope: &mut v8::HandleScope,
    #[proto] ro: &DOMPointReadOnly,
  ) -> f64 {
    *ro.y.get(scope)
  }

  #[setter]
  fn z(
    &self,
    scope: &mut v8::HandleScope,
    z: f64,
    #[proto] ro: &DOMPointReadOnly,
  ) {
    ro.z.set(scope, z);
  }

  #[getter]
  fn z(
    &self,
    scope: &mut v8::HandleScope,
    #[proto] ro: &DOMPointReadOnly,
  ) -> f64 {
    *ro.z.get(scope)
  }

  #[setter]
  fn w(
    &self,
    scope: &mut v8::HandleScope,
    w: f64,
    #[proto] ro: &DOMPointReadOnly,
  ) {
    ro.w.set(scope, w);
  }

  #[getter]
  fn w(
    &self,
    scope: &mut v8::HandleScope,
    #[proto] ro: &DOMPointReadOnly,
  ) -> f64 {
    *ro.w.get(scope)
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

unsafe impl GarbageCollected for TestEnumWrap {
  fn get_name(&self) -> &'static std::ffi::CStr {
    c"TestEnumWrap"
  }

  fn trace(&self, _visitor: &mut v8::cppgc::Visitor) {}
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

unsafe impl deno_core::GarbageCollected for Foo {
  fn get_name(&self) -> &'static std::ffi::CStr {
    c"Foo"
  }

  fn trace(&self, _visitor: &mut v8::cppgc::Visitor) {}
}

#[op2(fast)]
pub fn op_scope_cppgc(_scope: &mut v8::HandleScope, #[cppgc] _foo: &Foo) {}
