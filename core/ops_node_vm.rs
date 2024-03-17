// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

use crate::error::AnyError;
use crate::node_vm::contextify_context;
use crate::node_vm::make_context;
use crate::node_vm::ContextOptions;
use crate::node_vm::PRIVATE_SYMBOL_NAME;
use crate::op2;
use anyhow::bail;
use serde::Deserialize;
use v8::MapFnTo;

// TODO(bartlomieju): copy-pasted from Node, we probably shouldn't rely on these
// exact numbers.
pub const NODE_CONTEXT_EMBEDDER_DATA_INDEX: usize = 32;
pub const NODE_CONTEXT_SANDBOX_OBJECT_DATA_INDEX: usize = 33;
pub const NODE_CONTEXT_ALLOW_WASM_CODE_GENERATION_INDEX: usize = 34;
pub const NODE_BINDING_DATA_STORE_INDEX: usize = 35;
pub const NODE_CONTEXT_ALLOW_CODE_GENERATION_FROM_STRINGS_INDEX: usize = 36;
pub const NODE_CONTEXT_CONTEXTIFY_CONTEXT_INDEX: usize = 37;
pub const NODE_CONTEXT_REALM_INDEX: usize = 38;
// TODO(bartlomieju): figure out what this field does
// NODE_CONTEXT_TAG must be greater than any embedder indexes so that a single
// check on the number of embedder data fields can assure the presence of all
// embedder indexes.
pub const NODE_CONTEXT_TAG: usize = 39;

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
pub fn op_script_run_in_context(
  scope: &mut v8::HandleScope,
  script: v8::Local<v8::Value>,
  contextified_object: Option<v8::Local<v8::Object>>,
  #[smi] timeout: i32,
  display_errors: bool,
  break_on_signint: bool,
  break_first_line: bool,
) -> Result<(), AnyError> {
  // TODO: Node has `ContextifyScript` which is an actual object instance.
  // Probably will need to do the same here, probably using CPPGC objects.

  let context = if let Some(contextified_object) = contextified_object {
    let sandbox = contextified_object;
    // let contextify_context =
    todo!()
  } else {
    scope.get_current_context()
  };

  Ok(())
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
pub fn op_node_vm_script_new(scope: &mut v8::HandleScope) {}
