// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

use crate::error::AnyError;
use crate::op2;
use anyhow::bail;

pub const VM_CONTEXT_INDEX: usize = 0;

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

const OBJECT_STRING: &str = "Object";
const PRIVATE_SYMBOL_NAME: &[u8] = b"node:contextify:context";

struct SandboxObject(v8::Global<v8::Object>);
struct AllowCodeGenerationFromString(bool);
struct AllowWasmCodeGeneration(bool);

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

struct ContextOptions {
  name: String,
  origin: Option<String>,
  allow_code_gen_strings: bool,
  allow_code_gen_wasm: bool,
  // own_microtask_queue
  // host_defined_options_id
}

fn create_v8_context<'a>(
  scope: &mut v8::HandleScope<'a>,
  object_template: v8::Local<v8::ObjectTemplate>,
  snapshot_data: Option<&'static [u8]>,
  // microtask_queue,
) -> v8::Local<'a, v8::Context> {
  let scope = &mut v8::EscapableHandleScope::new(scope);

  let context = if let Some(snapshot_data) = snapshot_data {
    v8::Context::new_from_template(scope, object_template)
  } else {
    v8::Context::from_snapshot(scope, VM_CONTEXT_INDEX).unwrap()
  };

  scope.escape(context)
}

fn contextify_context_new(
  scope: &mut v8::HandleScope,
  v8_context: v8::Local<v8::Context>,
  sandbox_obj: v8::Local<v8::Object>,
  options: ContextOptions,
) -> Result<(), AnyError> {
  let main_context = scope.get_current_context();
  let new_context_global = v8_context.global(scope);
  v8_context.set_security_token(main_context.get_security_token(scope));

  // Store sandbox obj here
  let sandbox_obj_global = SandboxObject(v8::Global::new(scope, sandbox_obj));
  assert!(v8_context.set_slot(scope, sandbox_obj_global));

  v8_context.set_allow_generation_from_strings(false);
  assert!(v8_context.set_slot(
    scope,
    AllowCodeGenerationFromString(options.allow_code_gen_strings)
  ));
  assert!(v8_context
    .set_slot(scope, AllowWasmCodeGeneration(options.allow_code_gen_wasm)));

  // let info = ContextInfo { name: options.name };

  // let result;
  // let wrapper;

  {
    let mut context_scope = v8::ContextScope::new(scope, v8_context);
    let handle_scope = &mut v8::HandleScope::new(&mut context_scope);
    let ctor_name = sandbox_obj.get_constructor_name();
    let ctor_name_str = ctor_name.to_rust_string_lossy(handle_scope);
    if ctor_name_str != OBJECT_STRING {
      let key = v8::Symbol::get_to_string_tag(handle_scope);
      if new_context_global
        .define_own_property(
          handle_scope,
          key.into(),
          ctor_name.into(),
          v8::PropertyAttribute::DONT_ENUM,
        )
        .is_none()
      {
        bail!("Define new context's own property");
      }
    }

    // TODO: handle host_defined_options_id and dynamic import callback

    // TODO: assign to context - set up internal fields (not sure if needed), notify inspector about a new context
  }

  // TODO: uncomment once wrapper is done
  // let private_name =
  //   v8::String::new_external_onebyte_static(scope, PRIVATE_SYMBOL_NAME)
  //     .unwrap();
  // let private_symbol = v8::Private::for_api(scope, Some(private_name));
  // if sandbox_obj
  //   .set_private(scope, private_symbol, wrapper)
  //   .is_none()
  // {
  //   bail!("Set private property on contextified object");
  // };

  // TODO: assign host_defined_option_symbol to the wrapper.

  // result
  Ok(())
}

fn contextify_context(
  scope: &mut v8::HandleScope,
  sandbox: v8::Local<v8::Object>,
  options: ContextOptions,
) -> Result<(), AnyError> {
  let object_template = v8::ObjectTemplate::new(scope);
  // TODO: handle snapshot

  // TODO: handle microtask queue

  let v8_context = create_v8_context(
    scope,
    object_template,
    // snapshot_data
    None,
    // microtask queue
  );

  Ok(())
}

#[op2]
pub fn op_vm_make_context<'a>(
  scope: &mut v8::HandleScope<'a>,
  sandbox: v8::Local<v8::Object>,
  #[string] name: String,
  #[string] origin: Option<String>,
  allow_code_gen_strings: bool,
  allow_code_gen_wasm: bool,
  own_microtask_queue: bool,
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

  todo!();

  Ok(())
}
