// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct MathOp {
  pub a: u64,
  pub b: u64,
  pub operator: Option<String>,
}

fn main() {
  let platform = v8::new_default_platform(0, false).make_shared();
  v8::V8::initialize_platform(platform);
  v8::V8::initialize();

  {
    let isolate = &mut v8::Isolate::new(v8::CreateParams::default());
    let handle_scope = &mut v8::HandleScope::new(isolate);
    let context = v8::Context::new(handle_scope);
    let scope = &mut v8::ContextScope::new(handle_scope, context);

    fn exec<'s>(
      scope: &mut v8::HandleScope<'s>,
      src: &str,
    ) -> v8::Local<'s, v8::Value> {
      let code = v8::String::new(scope, src).unwrap();
      let script = v8::Script::compile(scope, code, None).unwrap();
      script.run(scope).unwrap()
    }

    let v = exec(scope, "32");
    let x32: u64 = serde_v8::from_v8(scope, v).unwrap();
    println!("x32 = {x32}");

    let v = exec(scope, "({a: 1, b: 3, c: 'ignored'})");
    let mop: MathOp = serde_v8::from_v8(scope, v).unwrap();
    println!(
      "mop = {{ a: {}, b: {}, operator: {:?} }}",
      mop.a, mop.b, mop.operator
    );

    let v = exec(scope, "[1,2,3,4,5]");
    let arr: Vec<u64> = serde_v8::from_v8(scope, v).unwrap();
    println!("arr = {arr:?}");

    let v = exec(scope, "['hello', 'world']");
    let hi: Vec<String> = serde_v8::from_v8(scope, v).unwrap();
    println!("hi = {hi:?}");

    let v: v8::Local<v8::Value> = v8::Number::new(scope, 12345.0).into();
    let x: f64 = serde_v8::from_v8(scope, v).unwrap();
    println!("x = {x}");
  }

  // SAFETY: all isolates have been destroyed, so we can now safely let V8 clean
  // up its resources.
  unsafe {
    v8::V8::dispose();
  }
  v8::V8::dispose_platform();
}
