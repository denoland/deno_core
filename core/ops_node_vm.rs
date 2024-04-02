// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

use crate::error::AnyError;
use crate::node_vm::ContextifyContext;
use crate::node_vm::contextify_context;
use crate::node_vm::make_context;
use crate::node_vm::ContextOptions;
use crate::node_vm::ContextifyScript;
use crate::node_vm::PRIVATE_SYMBOL_NAME;
use crate::op2;

#[op2]
pub fn op_vm_is_context(
  scope: &mut v8::HandleScope,
  sandbox: v8::Local<v8::Object>,
) -> bool {
  let private_name =
    v8::String::new_external_onebyte_static(scope, PRIVATE_SYMBOL_NAME)
      .unwrap();
  let private_symbol = v8::Private::for_api(scope, Some(private_name));
  sandbox.has_private(scope, private_symbol).unwrap()
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

#[op2]
pub fn op_script_run_in_context<'a>(
  scope: &mut v8::HandleScope<'a>,
  script: v8::Local<'a, v8::String>,
  contextified_object: Option<v8::Local<'a, v8::Object>>,
  #[smi] timeout: i32,
  display_errors: bool,
  break_on_signint: bool,
  break_first_line: bool,
) -> Result<v8::Local<'a, v8::Value>, AnyError> {
  // TODO: Node has `ContextifyScript` which is an actual object instance.
  // Probably will need to do the same here, probably using CPPGC objects.

  let context = if let Some(contextified_object) = contextified_object {
    let private_name =
      v8::String::new_external_onebyte_static(scope, PRIVATE_SYMBOL_NAME)
        .unwrap();
    let private_symbol = v8::Private::for_api(scope, Some(private_name));
    let private_value = contextified_object.get_private(scope, private_symbol).unwrap();
    let wrapper: v8::Local<v8::Object> = private_value.try_into()?;

    let contextify_context = unsafe {
        (wrapper.get_aligned_pointer_from_internal_field(0) as *const ContextifyContext)
.as_ref().unwrap()
    };
    v8::Local::new(scope, contextify_context.context.as_ref().unwrap())
  } else {
    scope.get_current_context()
  };

  let scope = &mut v8::ContextScope::new(scope, context);

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

#[op2]
pub fn op_vm_make_context(
  scope: &mut v8::HandleScope,
  sandbox: v8::Local<v8::Object>,
  #[string] name: String,
  #[string] origin: Option<String>,
  allow_code_gen_strings: bool,
  allow_code_gen_wasm: bool,
  // TODO(bartlomieju): missing bindings in rusty_v8 to support it
  // own_microtask_queue: bool,
  // TODO(bartlomieju):
  // host_defined_options_id
) -> Result<(), AnyError> {
  // Don't allow contextifying a sandbox multiple times.
  {
    let private_name =
      v8::String::new_external_onebyte_static(scope, PRIVATE_SYMBOL_NAME)
        .unwrap();
    let private_symbol = v8::Private::for_api(scope, Some(private_name));
    // TODO: this unwrap might be wrong
    assert!(!sandbox.has_private(scope, private_symbol).unwrap());
  }

  let options = ContextOptions {
    name,
    origin,
    allow_code_gen_strings,
    allow_code_gen_wasm,
  };

  contextify_context(scope, sandbox, options)?;
  // TODO(bartlomieju): v8 checks for try catch scope
  Ok(())
}

#[op2]
pub fn op_node_vm_script_new<'s>(
  scope: &'s mut v8::HandleScope,
  #[string] code: &str,
  #[string] filename: &str,
  #[smi] line_offset: i32,
  #[smi] column_offset: i32,
) -> Result<v8::Local<'s, v8::Object>, AnyError> {
  let script =
    ContextifyScript::new(scope, code, filename, line_offset, column_offset);
  let obj = crate::cppgc::make_cppgc_object(scope, script);
  Ok(obj)
}

#[op2]
pub fn op_node_vm_script_run_in_context<'s>(
  scope: &'s mut v8::HandleScope,
  #[cppgc] script: &ContextifyScript,
) -> Result<v8::Local<'s, v8::Value>, AnyError> {
  eprintln!("contextify script: {:?}", script);

  // TODO: make it work with `contextified_object`.
  let context = scope.get_current_context();

  let context_scope = &mut v8::ContextScope::new(scope, context);
  let mut scope = v8::EscapableHandleScope::new(context_scope);
  let result = script.eval_machine(&mut scope, context).unwrap();

  Ok(scope.escape(result))
}
