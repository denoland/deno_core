// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

use crate::error::AnyError;
use crate::op2;
use anyhow::bail;
use v8::MapFnTo;

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

#[derive(Debug, Clone)]
struct ContextifyContext {
  context: Option<v8::Global<v8::Context>>,
  wrapper: v8::Global<v8::Object>,
  sandbox: Option<v8::Global<v8::Object>>,
  // microtask_queue:
}

#[derive(Debug, Clone)]
struct SlotSandboxObject(v8::Global<v8::Object>);
#[derive(Debug, Clone)]
struct SlotAllowCodeGenerationFromString(bool);
#[derive(Debug, Clone)]
struct SlotAllowWasmCodeGeneration(bool);

#[derive(Debug, Clone)]
struct SlotContextifyContext(());
#[derive(Debug, Clone)]
struct SlotNodeContext;
#[derive(Debug, Clone)]
struct SlotContextifyGlobalTemplate(v8::Global<v8::ObjectTemplate>);
#[derive(Debug, Clone)]
struct SlotContextifyWrapperObjectTemplate(v8::Global<v8::ObjectTemplate>);

impl ContextifyContext {
  // TODO: maybe not needed?
  // fn contextify_context_get(
  //   scope: &mut v8::HandleScope,
  //   info: v8::PropertyCallbackInfo,
  // ) -> Option<ContextifyContext> {
  //   contextify_context_get_from_this(scope, info.this())
  // }

  fn context<'s>(
    &self,
    scope: &'s mut v8::HandleScope,
  ) -> v8::Local<'s, v8::Context> {
    let ctx = self.context.as_ref().unwrap();
    v8::Local::new(scope, ctx)
  }

  fn global_proxy<'s>(
    &self,
    scope: &'s mut v8::HandleScope,
  ) -> v8::Local<'s, v8::Object> {
    let ctx = self.context(scope);
    ctx.global(scope)
  }

  fn sandbox(&self, scope: &mut v8::HandleScope) -> v8::Local<v8::Object> {
    let sandbox = self.sandbox.as_ref().unwrap();
    v8::Local::new(scope, sandobox)
  }

  fn contextify_context_get_from_this(
    scope: &mut v8::HandleScope,
    object: v8::Local<v8::Object>,
  ) -> Option<ContextifyContext> {
    let Some(context) = object.get_creation_context(scope) else {
      return None;
    };

    // TODO(bartlomieju): do we really need this check?
    if context.get_slot::<SlotNodeContext>(scope).is_none() {
      return None;
    }

    // TODO(bartlomieju): maybe it should be an Rc?
    context.get_slot::<ContextifyContext>(scope).cloned()
  }

  fn is_still_initializing(
    maybe_contextify_context: Option<&ContextifyContext>,
  ) -> bool {
    match maybe_contextify_context {
      Some(ctx_ctx) => ctx_ctx.context.is_some(),
      None => false,
    }
  }
}

fn make_context<'a>(
  scope: &mut v8::HandleScope<'a>,
) -> v8::Local<'a, v8::Context> {
  let scope = &mut v8::EscapableHandleScope::new(scope);
  // let context = v8::Context::from_snapshot(scope, VM_CONTEXT_INDEX).unwrap();
  let context = v8::Context::new(scope);
  scope.escape(context)
}

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

struct ContextOptions {
  name: String,
  origin: Option<String>,
  allow_code_gen_strings: bool,
  allow_code_gen_wasm: bool,
  // TODO(bartlomieju): missing bindings in rusty_v8 to support it
  // own_microtask_queue:

  // TODO(bartlomieju):
  // host_defined_options_id
}

fn create_v8_context<'a>(
  scope: &mut v8::HandleScope<'a>,
  object_template: v8::Local<v8::ObjectTemplate>,
  snapshot_data: Option<&'static [u8]>,
  // TODO(bartlomieju): missing bindings in rusty_v8 to support it
  // microtask_queue,
) -> v8::Local<'a, v8::Context> {
  let scope = &mut v8::EscapableHandleScope::new(scope);

  let context = if let Some(snapshot_data) = snapshot_data {
    v8::Context::from_snapshot(scope, VM_CONTEXT_INDEX).unwrap()
  } else {
    // TODO(bartlomieju): missing bindings to pass `microtask_queue`.
    v8::Context::new_from_template(scope, object_template)
  };

  scope.escape(context)
}

fn contextify_context_new(
  scope: &mut v8::HandleScope,
  v8_context: v8::Local<v8::Context>,
  sandbox_obj: v8::Local<v8::Object>,
  options: ContextOptions,
) -> Result<ContextifyContext, AnyError> {
  let main_context = scope.get_current_context();
  let new_context_global = v8_context.global(scope);
  v8_context.set_security_token(main_context.get_security_token(scope));

  // Store sandbox obj here
  let sandbox_obj_global =
    SlotSandboxObject(v8::Global::new(scope, sandbox_obj));
  assert!(v8_context.set_slot(scope, sandbox_obj_global));

  v8_context.set_allow_generation_from_strings(false);
  assert!(v8_context.set_slot(
    scope,
    SlotAllowCodeGenerationFromString(options.allow_code_gen_strings)
  ));
  assert!(v8_context.set_slot(
    scope,
    SlotAllowWasmCodeGeneration(options.allow_code_gen_wasm)
  ));

  // TODO(bartlomieju): inspector support
  // let info = ContextInfo { name: options.name };
  let result = {
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

    // env->AssignToContext();
    // TODO: notify inspector about a new context
    {
      v8_context.set_slot(handle_scope, SlotContextifyContext(()));
      v8_context.set_slot(handle_scope, SlotNodeContext);
    }

    let template = handle_scope
      .get_slot::<SlotContextifyWrapperObjectTemplate>()
      .unwrap()
      .clone();
    let template_local = v8::Local::new(handle_scope, template.0);
    let Some(wrapper) = template_local.new_instance(handle_scope) else {
      bail!("Create new object instance");
    };

    let context_global = v8::Global::new(handle_scope, v8_context);
    let sandbox_obj_global = v8::Global::new(handle_scope, sandbox_obj);
    // TODO(bartlomieju): probably should be an Rc
    let contextify_context = ContextifyContext {
      context: Some(context_global),
      wrapper: v8::Global::new(handle_scope, wrapper),
      sandbox: Some(sandbox_obj_global),
    };
    assert!(v8_context.set_slot(handle_scope, contextify_context));
    let result = v8_context
      .get_slot::<ContextifyContext>(handle_scope)
      .unwrap()
      .clone();
    // TODO(bartlomieju): node makes context a weak handle here

    let private_name = v8::String::new_external_onebyte_static(
      handle_scope,
      PRIVATE_SYMBOL_NAME,
    )
    .unwrap();
    let private_symbol = v8::Private::for_api(handle_scope, Some(private_name));
    if sandbox_obj
      .set_private(handle_scope, private_symbol, wrapper.into())
      .is_none()
    {
      bail!("Set private property on contextified object");
    };
    result
  };
  // TODO: assign host_defined_option_symbol to the wrapper.

  Ok(result)
}

fn contextify_context(
  scope: &mut v8::HandleScope,
  sandbox: v8::Local<v8::Object>,
  options: ContextOptions,
) -> Result<(), AnyError> {
  // TODO(bartlomieju): I think we can check if this slot exists and run
  // `contextify_context_initialize_global_template` if it doesn't.
  let object_template_slot = scope
    .get_slot::<SlotContextifyGlobalTemplate>()
    .expect("ContextifyGlobalTemplate slot should be already populated.")
    .clone();
  let object_template = v8::Local::new(scope, object_template_slot.0);
  // TODO: handle snapshot

  // TODO: handle microtask queue

  let v8_context = create_v8_context(
    scope,
    object_template,
    // snapshot_data
    None,
    // microtask queue
  );

  {
    let context_scope = &mut v8::ContextScope::new(scope, v8_context);
    let mut scope = v8::HandleScope::new(context_scope);
    contextify_context_initialize_global_template(&mut scope);
  }

  contextify_context_new(scope, v8_context, sandbox, options)?;

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

extern "C" fn c_noop(info: *const v8::FunctionCallbackInfo) {}

// TODO(bartlomieju): this should be called once per `v8::Isolate` instance.
fn contextify_context_initialize_global_template(scope: &mut v8::HandleScope) {
  assert!(scope
    .get_slot::<SlotContextifyWrapperObjectTemplate>()
    .is_none());

  let global_func_template =
    v8::FunctionTemplate::builder_raw(c_noop).build(scope);
  let global_object_template = global_func_template.instance_template(scope);

  let named_property_handler_config = {
    let mut config = v8::NamedPropertyHandlerConfiguration::new()
      .flags(v8::PropertyHandlerFlags::HAS_NO_SIDE_EFFECT);

    // TODO: use thread locals to avoid rustc bug
    config = config.getter_raw(property_getter.map_fn_to());
    config = config.setter_raw(property_setter.map_fn_to());
    config = config.descriptor_raw(property_descriptor.map_fn_to());
    config = config.deleter_raw(property_deleter.map_fn_to());
    config = config.enumerator_raw(property_enumerator.map_fn_to());
    config = config.definer_raw(property_definer.map_fn_to());
    config
  };

  let indexed_property_handler_config = {
    let mut config = v8::IndexedPropertyHandlerConfiguration::new()
      .flags(v8::PropertyHandlerFlags::HAS_NO_SIDE_EFFECT);

    // TODO: use thread locals to avoid rustc bug
    config = config.getter_raw(indexed_property_getter.map_fn_to());
    config = config.setter_raw(indexed_property_setter.map_fn_to());
    config = config.descriptor_raw(indexed_property_descriptor.map_fn_to());
    config = config.deleter_raw(indexed_property_deleter.map_fn_to());
    config = config.enumerator_raw(property_enumerator.map_fn_to());
    config = config.definer_raw(indexed_property_definer.map_fn_to());
    config
  };

  global_object_template
    .set_named_property_handler(named_property_handler_config);
  global_object_template
    .set_indexed_property_handler(indexed_property_handler_config);
  let contextify_global_template_slot = SlotContextifyGlobalTemplate(
    v8::Global::new(scope, global_object_template),
  );
  scope.set_slot(contextify_global_template_slot);

  // TODO:
  // Local<FunctionTemplate> wrapper_func_template =
  //     BaseObject::MakeLazilyInitializedJSTemplate(isolate_data);
  let wrapper_func_template =
    v8::FunctionTemplate::builder_raw(c_noop).build(scope);

  let wrapper_object_template = wrapper_func_template.instance_template(scope);
  let wrapper_object_template_global =
    v8::Global::new(scope, wrapper_object_template);
  scope.set_slot(SlotContextifyWrapperObjectTemplate(
    wrapper_object_template_global,
  ));
}

pub fn property_getter<'s>(
  scope: &mut v8::HandleScope<'s>,
  key: v8::Local<'s, v8::Name>,
  args: v8::PropertyCallbackArguments<'s>,
  mut rv: v8::ReturnValue,
) {
  let ctx =
    ContextifyContext::contextify_context_get_from_this(scope, args.this());

  if ContextifyContext::is_still_initializing(ctx.as_ref()) {
    return;
  }

  let ctx = ctx.unwrap();
  // let context = ctx.context;
  let sandbox = ctx.sandbox(scope);
  let mut maybe_rv = sandbox.get_real_named_property(scope, key);

  if maybe_rv.is_none() {
    maybe_rv = ctx.global_proxy(scope).get_real_named_property(scope, key);
  }

  if let Some(mut rv_) = maybe_rv {
    if rv_ == sandbox {
      rv_ = ctx.global_proxy(scope).into();
    }

    rv.set(rv_);
  }
}

pub fn property_setter<'s>(
  scope: &mut v8::HandleScope<'s>,
  key: v8::Local<'s, v8::Name>,
  value: v8::Local<'s, v8::Value>,
  args: v8::PropertyCallbackArguments<'s>,
  mut rv: v8::ReturnValue,
) {
  let ctx =
    ContextifyContext::contextify_context_get_from_this(scope, args.this());

  if ContextifyContext::is_still_initializing(ctx.as_ref()) {
    return;
  }

  let ctx = ctx.unwrap();
  let context = ctx.context(scope);
  let (attributes, is_declared_on_global_proxy) = match ctx
    .global_proxy(scope)
    .get_real_named_property_attributes(scope, key)
  {
    Some(attr) => (attr, true),
    None => (v8::PropertyAttribute::NONE, false),
  };
  let mut read_only = attributes.is_read_only();

  let (attributes, is_declared_on_sandbox) = match ctx
    .sandbox(scope)
    .get_real_named_property_attributes(scope, key)
  {
    Some(attr) => (attr, true),
    None => (v8::PropertyAttribute::NONE, false),
  };
  read_only |= attributes.is_read_only();

  if read_only {
    return;
  }

  // true for x = 5
  // false for this.x = 5
  // false for Object.defineProperty(this, 'foo', ...)
  // false for vmResult.x = 5 where vmResult = vm.runInContext();
  let is_contextual_store = ctx.global_proxy(scope) != args.this();

  // Indicator to not return before setting (undeclared) function declarations
  // on the sandbox in strict mode, i.e. args.ShouldThrowOnError() = true.
  // True for 'function f() {}', 'this.f = function() {}',
  // 'var f = function()'.
  // In effect only for 'function f() {}' because
  // var f = function(), is_declared = true
  // this.f = function() {}, is_contextual_store = false.
  let is_function = value.is_function();

  let is_declared = is_declared_on_global_proxy || is_declared_on_sandbox;
  if !is_declared
    && args.should_throw_on_error()
    && is_contextual_store
    && !is_function
  {
    return;
  }

  if !is_declared && key.is_symbol() {
    return;
  };

  if ctx.sandbox(scope).set(scope, key.into(), value).is_none() {
    return;
  }

  if is_declared_on_sandbox {
    if let Some(desc) =
      ctx.sandbox(scope).get_own_property_descriptor(scope, key)
    {
      if !desc.is_undefined() {
        let desc_obj: v8::Local<v8::Object> = desc.try_into().unwrap();
        // We have to specify the return value for any contextual or get/set
        // property
        let get_key =
          v8::String::new_external_onebyte_static(scope, b"get").unwrap();
        let set_key =
          v8::String::new_external_onebyte_static(scope, b"get").unwrap();
        if desc_obj
          .has_own_property(scope, get_key.into())
          .unwrap_or(false)
          || desc_obj
            .has_own_property(scope, set_key.into())
            .unwrap_or(false)
        {
          rv.set(value);
        }
      }
    }
  }
}

pub fn property_deleter<'s>(
  scope: &mut v8::HandleScope<'s>,
  key: v8::Local<'s, v8::Name>,
  args: v8::PropertyCallbackArguments<'s>,
  mut rv: v8::ReturnValue,
) {
  let ctx =
    ContextifyContext::contextify_context_get_from_this(scope, args.this());

  if ContextifyContext::is_still_initializing(ctx.as_ref()) {
    return;
  }

  let ctx = ctx.unwrap();
  // TODO: should use a scope created from `context`?
  // let context = ctx.context.unwrap();
  let sandbox = ctx.sandbox(scope);
  let success = sandbox.delete(scope, key.into()).unwrap_or(false);

  if success {
    return;
  }

  // Delete failed on the sandbox, intercept and do not delete on
  // the global object.
  rv.set_bool(false);
}

pub fn property_enumerator<'s>(
  scope: &mut v8::HandleScope<'s>,
  args: v8::PropertyCallbackArguments<'s>,
  mut rv: v8::ReturnValue,
) {
  let ctx =
    ContextifyContext::contextify_context_get_from_this(scope, args.this());

  if ContextifyContext::is_still_initializing(ctx.as_ref()) {
    return;
  }

  let ctx = ctx.unwrap();
  // TODO: should use a scope created from `context`?
  // let context = ctx.context.unwrap();
  let sandbox = ctx.sandbox(scope);
  let Some(properties) = sandbox
    .get_property_names(scope, v8::GetPropertyNamesArgsBuilder::new().build())
  else {
    return;
  };

  rv.set(properties.into());
}

pub fn property_definer<'s>(
  scope: &mut v8::HandleScope<'s>,
  key: v8::Local<'s, v8::Name>,
  descriptor: &v8::PropertyDescriptor,
  args: v8::PropertyCallbackArguments<'s>,
  mut rv: v8::ReturnValue,
) {
  let ctx =
    ContextifyContext::contextify_context_get_from_this(scope, args.this());

  if ContextifyContext::is_still_initializing(ctx.as_ref()) {
    return;
  }

  let ctx = ctx.unwrap();
  let context = ctx.context(scope);
  let (attributes, is_declared) = match ctx
    .global_proxy(scope)
    .get_real_named_property_attributes(scope, key)
  {
    Some(attr) => (attr, true),
    None => (v8::PropertyAttribute::NONE, false),
  };
  let mut read_only = attributes.is_read_only();

  // If the property is set on the global as read_only, don't change it on
  // the global or sandbox.
  if is_declared && read_only {
    return;
  }

  // TODO:
  // Local<Object> sandbox = ctx->sandbox();

  // auto cargo run =
  //     [&] (PropertyDescriptor* desc_for_sandbox) {
  //       if (desc.has_enumerable()) {
  //         desc_for_sandbox->set_enumerable(desc.enumerable());
  //       }
  //       if (desc.has_configurable()) {
  //         desc_for_sandbox->set_configurable(desc.configurable());
  //       }
  //       // Set the property on the sandbox.
  //       USE(sandbox->DefineProperty(context, property, *desc_for_sandbox));
  //     };

  // if (desc.has_get() || desc.has_set()) {
  //   PropertyDescriptor desc_for_sandbox(
  //       desc.has_get() ? desc.get() : Undefined(isolate).As<Value>(),
  //       desc.has_set() ? desc.set() : Undefined(isolate).As<Value>());

  //   define_prop_on_sandbox(&desc_for_sandbox);
  // } else {
  //   Local<Value> value =
  //       desc.has_value() ? desc.value() : Undefined(isolate).As<Value>();

  //   if (desc.has_writable()) {
  //     PropertyDescriptor desc_for_sandbox(value, desc.writable());
  //     define_prop_on_sandbox(&desc_for_sandbox);
  //   } else {
  //     PropertyDescriptor desc_for_sandbox(value);
  //     define_prop_on_sandbox(&desc_for_sandbox);
  //   }
  // }
}

pub fn property_descriptor<'s>(
  scope: &mut v8::HandleScope<'s>,
  key: v8::Local<'s, v8::Name>,
  args: v8::PropertyCallbackArguments<'s>,
  mut rv: v8::ReturnValue,
) {
  let ctx =
    ContextifyContext::contextify_context_get_from_this(scope, args.this());

  if ContextifyContext::is_still_initializing(ctx.as_ref()) {
    return;
  }

  let ctx = ctx.unwrap();
  // TODO: should use a scope created from `context`?
  // let context = ctx.context.unwrap();
  let sandbox = ctx.sandbox(scope);

  if sandbox.has_own_property(scope, key).unwrap_or(false) {
    if let Some(desc) = sandbox.get_own_property_descriptor(scope, key) {
      rv.set(desc);
    }
  }
}

fn uint32_to_name<'s>(
  scope: &mut v8::HandleScope<'s>,
  index: u32,
) -> v8::Local<'s, v8::Name> {
  let int = v8::Integer::new_from_unsigned(scope, index);
  let u32 = v8::Local::<v8::Uint32>::try_from(int).unwrap();
  u32.to_string(scope).unwrap().into()
}

fn indexed_property_getter<'s>(
  scope: &mut v8::HandleScope<'s>,
  index: u32,
  args: v8::PropertyCallbackArguments<'s>,
  mut rv: v8::ReturnValue,
) {
  let ctx =
    ContextifyContext::contextify_context_get_from_this(scope, args.this());

  if ContextifyContext::is_still_initializing(ctx.as_ref()) {
    return;
  }

  let key = uint32_to_name(scope, index);
  property_getter(scope, key, args, rv);
}

fn indexed_property_setter<'s>(
  scope: &mut v8::HandleScope<'s>,
  index: u32,
  value: v8::Local<'s, v8::Value>,
  args: v8::PropertyCallbackArguments<'s>,
  mut rv: v8::ReturnValue,
) {
  let ctx =
    ContextifyContext::contextify_context_get_from_this(scope, args.this());

  if ContextifyContext::is_still_initializing(ctx.as_ref()) {
    return;
  }

  let key = uint32_to_name(scope, index);
  property_setter(scope, key, value, args, rv);
}

fn indexed_property_deleter<'s>(
  scope: &mut v8::HandleScope<'s>,
  index: u32,
  args: v8::PropertyCallbackArguments<'s>,
  mut rv: v8::ReturnValue,
) {
  let ctx =
    ContextifyContext::contextify_context_get_from_this(scope, args.this());

  if ContextifyContext::is_still_initializing(ctx.as_ref()) {
    return;
  }

  let ctx = ctx.unwrap();
  // TODO: should use a scope created from `context`?
  // let context = ctx.context.unwrap();
  let sandbox = ctx.sandbox(scope);
  let key = uint32_to_name(scope, index);
  let success = sandbox.delete(scope, key.into()).unwrap_or(false);

  if success {
    return;
  }

  // Delete failed on the sandbox, intercept and do not delete on
  // the global object.
  rv.set_bool(false);
}

fn indexed_property_definer<'s>(
  scope: &mut v8::HandleScope<'s>,
  index: u32,
  descriptor: &v8::PropertyDescriptor,
  args: v8::PropertyCallbackArguments<'s>,
  mut rv: v8::ReturnValue,
) {
  let ctx =
    ContextifyContext::contextify_context_get_from_this(scope, args.this());

  if ContextifyContext::is_still_initializing(ctx.as_ref()) {
    return;
  }

  let key = uint32_to_name(scope, index);
  property_definer(scope, key, descriptor, args, rv);
}

fn indexed_property_descriptor<'s>(
  scope: &mut v8::HandleScope<'s>,
  index: u32,
  args: v8::PropertyCallbackArguments<'s>,
  mut rv: v8::ReturnValue,
) {
  let ctx =
    ContextifyContext::contextify_context_get_from_this(scope, args.this());

  if ContextifyContext::is_still_initializing(ctx.as_ref()) {
    return;
  }

  let key = uint32_to_name(scope, index);
  property_descriptor(scope, key, args, rv);
}
