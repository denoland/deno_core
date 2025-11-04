// Copyright 2018-2025 the Deno authors. MIT license.

use std::cell::RefCell;
use std::rc::Rc;

use deno_core::FromV8;
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
  fn trace(&self, _visitor: &mut v8::cppgc::Visitor) {}

  fn get_name(&self) -> &'static std::ffi::CStr {
    c"TestObjectWrap"
  }
}

fn int(
  _scope: &mut v8::PinScope,
  value: v8::Local<v8::Value>,
) -> Result<(), JsErrorBox> {
  if value.is_int32() {
    return Ok(());
  }

  Err(JsErrorBox::type_error("Expected int"))
}

fn int_op(
  _scope: &mut v8::PinScope,
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
  fn with_scope_fast(&self, _scope: &mut v8::PinScope) {}

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
  fn trace(&self, _visitor: &mut v8::cppgc::Visitor) {}

  fn get_name(&self) -> &'static std::ffi::CStr {
    c"DOMPoint"
  }
}

impl DOMPointReadOnly {
  fn from_point_inner(
    scope: &mut v8::PinScope,
    other: v8::Local<v8::Object>,
  ) -> Result<DOMPointReadOnly, JsErrorBox> {
    fn get(
      scope: &mut v8::PinScope,
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
  fn trace(&self, _visitor: &mut v8::cppgc::Visitor) {}

  fn get_name(&self) -> &'static std::ffi::CStr {
    c"DOMPointReadOnly"
  }
}

#[op2(base)]
impl DOMPointReadOnly {
  #[getter]
  fn x(&self, isolate: &v8::Isolate) -> f64 {
    *self.x.get(isolate)
  }

  #[getter]
  fn y(&self, isolate: &v8::Isolate) -> f64 {
    *self.y.get(isolate)
  }

  #[getter]
  fn z(&self, isolate: &v8::Isolate) -> f64 {
    *self.z.get(isolate)
  }

  #[getter]
  fn w(&self, isolate: &v8::Isolate) -> f64 {
    *self.w.get(isolate)
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
    scope: &mut v8::PinScope,
    other: v8::Local<v8::Object>,
  ) -> Result<DOMPointReadOnly, JsErrorBox> {
    DOMPointReadOnly::from_point_inner(scope, other)
  }

  #[required(1)]
  #[cppgc]
  fn from_point(
    &self,
    scope: &mut v8::PinScope,
    other: v8::Local<v8::Object>,
  ) -> Result<DOMPointReadOnly, JsErrorBox> {
    DOMPointReadOnly::from_point_inner(scope, other)
  }

  #[setter]
  fn x(
    &self,
    isolate: &mut v8::Isolate,
    x: f64,
    #[proto] ro: &DOMPointReadOnly,
  ) {
    ro.x.set(isolate, x);
  }

  #[getter]
  fn x(&self, isolate: &v8::Isolate, #[proto] ro: &DOMPointReadOnly) -> f64 {
    *ro.x.get(isolate)
  }

  #[setter]
  fn y(
    &self,
    isolate: &mut v8::Isolate,
    y: f64,
    #[proto] ro: &DOMPointReadOnly,
  ) {
    ro.y.set(isolate, y);
  }

  #[getter]
  fn y(&self, isolate: &v8::Isolate, #[proto] ro: &DOMPointReadOnly) -> f64 {
    *ro.y.get(isolate)
  }

  #[setter]
  fn z(
    &self,
    isolate: &mut v8::Isolate,
    z: f64,
    #[proto] ro: &DOMPointReadOnly,
  ) {
    ro.z.set(isolate, z);
  }

  #[getter]
  fn z(&self, isolate: &v8::Isolate, #[proto] ro: &DOMPointReadOnly) -> f64 {
    *ro.z.get(isolate)
  }

  #[setter]
  fn w(
    &self,
    isolate: &mut v8::Isolate,
    w: f64,
    #[proto] ro: &DOMPointReadOnly,
  ) {
    ro.w.set(isolate, w);
  }

  #[getter]
  fn w(&self, isolate: &v8::Isolate, #[proto] ro: &DOMPointReadOnly) -> f64 {
    *ro.w.get(isolate)
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

  #[fast]
  #[rename("impl")]
  fn impl_method(&self) {}
}

#[repr(u8)]
#[derive(Clone, Copy)]
pub enum TestEnumWrap {
  #[allow(dead_code)]
  A,
}

unsafe impl GarbageCollected for TestEnumWrap {
  fn trace(&self, _visitor: &mut v8::cppgc::Visitor) {}

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

#[op2(fast)]
pub fn op_thingy<'a>(_scope: &mut v8::PinScope<'a, '_>, thing: v8::Local<'a, v8::Value>) {
  let thing = thing.cast::<v8::Uint16Array>();
  let data = thing.data();
  println!(
    "data: {:?}; length: {:?}; byte offset: {:?}; byte length: {:?};",
    data,
    thing.length(),
    thing.byte_offset(),
    thing.byte_length()
  );
  if data.align_offset(align_of::<u16>()) != 0 {
    panic!("data is not aligned");
  }

  let data = unsafe { data.add(thing.byte_offset()) };

  let mut dest: Vec<u16> =
    vec![0; thing.byte_length() / std::mem::size_of::<u16>()];
  let dest_ptr = dest.as_mut_ptr();
  unsafe {
    std::ptr::copy_nonoverlapping(
      data.cast::<u16>(),
      dest_ptr.cast::<u16>(),
      thing.byte_length(),
    );
  }

  eprintln!("dest: {:?}", dest);

  let from_v8 = Vec::<u16>::from_v8(_scope, thing.into()).unwrap();
  eprintln!("from_v8: {:?}", from_v8);
}

// /// Transmutes a `Vec` of one type to a `Vec` of another type.
// ///
// /// # Safety
// /// `T` must be transmutable to `U`
// unsafe fn transmute_vec<T, U>(v: Vec<T>) -> Vec<U> {
//   const {
//     assert!(std::mem::size_of::<T>() == std::mem::size_of::<U>());
//     assert!(std::mem::align_of::<T>() == std::mem::align_of::<U>());
//   }

//   // make sure the original vector is not dropped
//   let mut v = std::mem::ManuallyDrop::new(v);
//   let len = v.len();
//   let cap = v.capacity();
//   let ptr = v.as_mut_ptr();

//   // SAFETY: the original vector is not dropped, the caller upholds the
//   // transmutability invariants, and the length and capacity are not changed.
//   unsafe { Vec::from_raw_parts(ptr as *mut U, len, cap) }
// }
