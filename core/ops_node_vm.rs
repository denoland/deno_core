// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
use crate::error::AnyError;
use crate::op2;

pub const VM_CONTEXT_INDEX: usize = 0;

fn make_context<'a>(
  scope: &mut v8::HandleScope<'a>,
) -> v8::Local<'a, v8::Context> {
  let scope = &mut v8::EscapableHandleScope::new(scope);
  // let context = v8::Context::from_snapshot(scope, VM_CONTEXT_INDEX).unwrap();
  let context = v8::Context::new(scope);
  scope.escape(context)
}

#[op2]
pub fn op_vm_run_in_new_context<'a>(
  scope: &mut v8::HandleScope<'a>,
  script: v8::Local<v8::String>,
  ctx_val: v8::Local<v8::Value>,
) -> Result<v8::Local<'a, v8::Value>, AnyError> {
  let _ctx_obj = if ctx_val.is_undefined() || ctx_val.is_null() {
    v8::Object::new(scope)
  } else {
    ctx_val.try_into()?
  };

  let ctx = make_context(scope);

  let scope = &mut v8::ContextScope::new(scope, ctx);

  let tc_scope = &mut v8::TryCatch::new(scope);
  let script = match v8::Script::compile(tc_scope, script, None) {
    Some(s) => s,
    None => {
      assert!(tc_scope.has_caught());
      tc_scope.rethrow();
      return Ok(v8::undefined(tc_scope).into());
    }
  };

  Ok(match script.run(tc_scope) {
    Some(result) => result,
    None => {
      assert!(tc_scope.has_caught());
      tc_scope.rethrow();

      v8::undefined(tc_scope).into()
    }
  })
}
