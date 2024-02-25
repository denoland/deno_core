// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use anyhow::Context;
use log::debug;
use std::os::raw::c_void;
use std::path::PathBuf;
use url::Url;
use v8::MapFnTo;

use super::jsruntime::BUILTIN_SOURCES;
use super::jsruntime::CONTEXT_SETUP_SOURCES;
use crate::error::has_call_site;
use crate::error::is_instance_of_error;
use crate::error::throw_type_error;
use crate::error::AnyError;
use crate::error::JsStackFrame;
use crate::modules::get_requested_module_type_from_attributes;
use crate::modules::parse_import_attributes;
use crate::modules::synthetic_module_evaluation_steps;
use crate::modules::ImportAttributesKind;
use crate::modules::ModuleMap;
use crate::ops::OpCtx;
use crate::runtime::InitMode;
use crate::runtime::JsRealm;
use crate::FastString;
use crate::JsRuntime;
use crate::ModuleType;

pub(crate) fn create_external_references(
  ops: &[OpCtx],
  additional_references: &[v8::ExternalReference],
) -> &'static v8::ExternalReferences {
  // Overallocate a bit, it's better than having to resize the vector.
  let mut references = Vec::with_capacity(
    6 + CONTEXT_SETUP_SOURCES.len()
      + BUILTIN_SOURCES.len()
      + (ops.len() * 4)
      + additional_references.len(),
  );

  references.push(v8::ExternalReference {
    function: call_console.map_fn_to(),
  });
  references.push(v8::ExternalReference {
    function: import_meta_resolve.map_fn_to(),
  });
  references.push(v8::ExternalReference {
    function: catch_dynamic_import_promise_error.map_fn_to(),
  });
  references.push(v8::ExternalReference {
    function: empty_fn.map_fn_to(),
  });
  references.push(v8::ExternalReference {
    function: op_disabled_fn.map_fn_to(),
  });
  let syn_module_eval_fn: v8::SyntheticModuleEvaluationSteps =
    synthetic_module_evaluation_steps.map_fn_to();
  references.push(v8::ExternalReference {
    pointer: syn_module_eval_fn as *mut c_void,
  });

  // Using v8::OneByteConst and passing external references to it
  // allows V8 to take an optimized path when deserializing the snapshot.
  for source_file in &CONTEXT_SETUP_SOURCES {
    references.push(v8::ExternalReference {
      pointer: source_file.source_onebyte_const as *const v8::OneByteConst
        as *mut c_void,
    });
  }

  for source_file in &BUILTIN_SOURCES {
    references.push(v8::ExternalReference {
      pointer: source_file.source_onebyte_const as *const v8::OneByteConst
        as *mut c_void,
    });
  }

  for ctx in ops {
    if ctx.metrics_enabled() {
      let ctx_ptr = ctx as *const OpCtx as _;
      references.push(v8::ExternalReference { pointer: ctx_ptr });
      references.push(v8::ExternalReference {
        function: ctx.decl.slow_fn_with_metrics,
      });
      if let Some(fast_fn) = &ctx.decl.fast_fn_with_metrics {
        references.push(v8::ExternalReference {
          pointer: fast_fn.function as _,
        });
        references.push(v8::ExternalReference {
          pointer: ctx.fast_fn_c_info.unwrap().as_ptr() as _,
        });
      }
    } else {
      let ctx_ptr = ctx as *const OpCtx as _;
      references.push(v8::ExternalReference { pointer: ctx_ptr });
      references.push(v8::ExternalReference {
        function: ctx.decl.slow_fn,
      });
      if let Some(fast_fn) = &ctx.decl.fast_fn {
        references.push(v8::ExternalReference {
          pointer: fast_fn.function as _,
        });
        references.push(v8::ExternalReference {
          pointer: ctx.fast_fn_c_info.unwrap().as_ptr() as _,
        });
      }
    }
  }

  references.extend_from_slice(additional_references);

  let refs = v8::ExternalReferences::new(&references);
  let refs: &'static v8::ExternalReferences = Box::leak(Box::new(refs));
  refs
}

// TODO(nayeemrmn): Move to runtime and/or make `pub(crate)`.
pub fn script_origin<'a>(
  s: &mut v8::HandleScope<'a>,
  resource_name: v8::Local<'a, v8::String>,
) -> v8::ScriptOrigin<'a> {
  let source_map_url = v8::String::empty(s);
  v8::ScriptOrigin::new(
    s,
    resource_name.into(),
    0,
    0,
    false,
    123,
    source_map_url.into(),
    true,
    false,
    false,
  )
}

pub(crate) fn get<'s, T>(
  scope: &mut v8::HandleScope<'s>,
  from: v8::Local<v8::Object>,
  key: &'static v8::OneByteConst,
  path: &'static str,
) -> T
where
  v8::Local<'s, v8::Value>: TryInto<T>,
{
  let key = v8_static_strings::new(scope, key);
  from
    .get(scope, key.into())
    .unwrap_or_else(|| panic!("{path} exists"))
    .try_into()
    .unwrap_or_else(|_| panic!("unable to convert"))
}

pub mod v8_static_strings {
  pub fn new<'s>(
    scope: &mut v8::HandleScope<'s>,
    str_: &'static v8::OneByteConst,
  ) -> v8::Local<'s, v8::String> {
    v8::String::new_from_onebyte_const(scope, str_).unwrap()
  }

  pub fn new_from_static_str<'s>(
    scope: &mut v8::HandleScope<'s>,
    str_: &'static [u8],
  ) -> v8::Local<'s, v8::String> {
    v8::String::new_external_onebyte_static(scope, str_).unwrap()
  }

  macro_rules! onebyte_const {
    ($str_:literal) => {
      v8::String::create_external_onebyte_const($str_.as_bytes())
    };
  }

  pub static DENO: v8::OneByteConst = onebyte_const!("Deno");
  pub static CORE: v8::OneByteConst = onebyte_const!("core");
  pub static OPS: v8::OneByteConst = onebyte_const!("ops");
  pub static URL: v8::OneByteConst = onebyte_const!("url");
  pub static MAIN: v8::OneByteConst = onebyte_const!("main");
  pub static RESOLVE: v8::OneByteConst = onebyte_const!("resolve");
  pub static MESSAGE: v8::OneByteConst = onebyte_const!("message");
  pub static CODE: v8::OneByteConst = onebyte_const!("code");
  pub static ERR_MODULE_NOT_FOUND: v8::OneByteConst =
    onebyte_const!("ERR_MODULE_NOT_FOUND");
  pub static EVENT_LOOP_TICK: v8::OneByteConst =
    onebyte_const!("eventLoopTick");
  pub static BUILD_CUSTOM_ERROR: v8::OneByteConst =
    onebyte_const!("buildCustomError");
  pub static CONSOLE: v8::OneByteConst = onebyte_const!("console");
  pub static CALL_CONSOLE: v8::OneByteConst = onebyte_const!("callConsole");
  pub static FILENAME: v8::OneByteConst = onebyte_const!("filename");
  pub static DIRNAME: v8::OneByteConst = onebyte_const!("dirname");
  pub static SET_UP_ASYNC_STUB: v8::OneByteConst =
    onebyte_const!("setUpAsyncStub");
  pub static WEBASSEMBLY: v8::OneByteConst = onebyte_const!("WebAssembly");
  pub static INSTANTIATE: v8::OneByteConst = onebyte_const!("instantiate");
  pub static WASM_INSTANTIATE: v8::OneByteConst =
    onebyte_const!("wasmInstantiate");
}

/// Create an object on the `globalThis` that looks like this:
/// ```ignore
/// globalThis.Deno = {
///   core: {
///     ops: {},
///   },
///   // console from V8
///   console,
///   // wrapper fn to forward message to V8 console
///   callConsole,
/// };
/// ```
pub(crate) fn initialize_deno_core_namespace<'s>(
  scope: &mut v8::HandleScope<'s>,
  context: v8::Local<'s, v8::Context>,
  init_mode: InitMode,
) {
  let global = context.global(scope);
  let deno_str = v8_static_strings::new(scope, &v8_static_strings::DENO);

  let maybe_deno_obj_val = global.get(scope, deno_str.into());

  // If `Deno.core` is already set up, let's exit early.
  if let Some(deno_obj_val) = maybe_deno_obj_val {
    if !deno_obj_val.is_undefined() {
      return;
    }
  }

  let deno_obj = v8::Object::new(scope);
  let deno_core_key = v8_static_strings::new(scope, &v8_static_strings::CORE);
  // Set up `Deno.core.ops` object
  let deno_core_ops_obj = v8::Object::new(scope);
  let deno_core_ops_key =
    v8_static_strings::new(scope, &v8_static_strings::OPS);

  let deno_core_obj = v8::Object::new(scope);
  deno_core_obj
    .set(scope, deno_core_ops_key.into(), deno_core_ops_obj.into())
    .unwrap();

  // If we're initializing fresh context set up the console
  if init_mode == InitMode::New {
    // Bind `call_console` to Deno.core.callConsole
    let call_console_fn = v8::Function::new(scope, call_console).unwrap();
    let call_console_key =
      v8_static_strings::new(scope, &v8_static_strings::CALL_CONSOLE);
    deno_core_obj.set(scope, call_console_key.into(), call_console_fn.into());

    // Bind v8 console object to Deno.core.console
    let extra_binding_obj = context.get_extras_binding_object(scope);
    let console_obj: v8::Local<v8::Object> = get(
      scope,
      extra_binding_obj,
      &v8_static_strings::CONSOLE,
      "ExtrasBindingObject.console",
    );
    let console_key =
      v8_static_strings::new(scope, &v8_static_strings::CONSOLE);
    deno_core_obj.set(scope, console_key.into(), console_obj.into());
  }

  deno_obj.set(scope, deno_core_key.into(), deno_core_obj.into());
  global.set(scope, deno_str.into(), deno_obj.into());
}

/// Execute `00_primordials.js` and `00_infra.js` that are required for ops
/// to function properly
pub(crate) fn initialize_primordials_and_infra(
  scope: &mut v8::HandleScope,
) -> Result<(), AnyError> {
  for source_file in &CONTEXT_SETUP_SOURCES {
    let name = v8::String::new_from_onebyte_const(
      scope,
      &source_file.specifier_onebyte_const,
    )
    .unwrap();
    let source_str = v8::String::new_from_onebyte_const(
      scope,
      &source_file.source_onebyte_const,
    )
    .unwrap();

    let origin = script_origin(scope, name);
    // TODO(bartlomieju): these two calls will panic if there's any problem in the JS code
    let script = v8::Script::compile(scope, source_str, Some(&origin))
      .with_context(|| format!("Failed to parse {}", source_file.specifier))?;
    script.run(scope).with_context(|| {
      format!("Failed to execute {}", source_file.specifier)
    })?;
  }

  Ok(())
}

/// Set up JavaScript bindings for ops.
pub(crate) fn initialize_deno_core_ops_bindings<'s>(
  scope: &mut v8::HandleScope<'s>,
  context: v8::Local<'s, v8::Context>,
  op_ctxs: &[OpCtx],
) {
  let global = context.global(scope);

  // Set up JavaScript bindings for the defined op - this will insert proper
  // `v8::Function` into `Deno.core.ops` object. For async ops, there a bit
  // more machinery involved, see comment below.
  let deno_obj = get(scope, global, &v8_static_strings::DENO, "Deno");
  let deno_core_obj =
    get(scope, deno_obj, &v8_static_strings::CORE, "Deno.core");
  let deno_core_ops_obj: v8::Local<v8::Object> = get(
    scope,
    deno_core_obj,
    &v8_static_strings::OPS,
    "Deno.core.ops",
  );
  let set_up_async_stub_fn: v8::Local<v8::Function> = get(
    scope,
    deno_core_obj,
    &v8_static_strings::SET_UP_ASYNC_STUB,
    "Deno.core.setUpAsyncStub",
  );

  let undefined = v8::undefined(scope);
  for op_ctx in op_ctxs {
    let mut op_fn = op_ctx_function(scope, op_ctx);
    let key = v8_static_strings::new_from_static_str(
      scope,
      op_ctx.decl.name.as_bytes(),
    );

    // For async ops we need to set them up, by calling `Deno.core.setUpAsyncStub` -
    // this call will generate an optimized function that binds to the provided
    // op, while keeping track of promises and error remapping.
    if op_ctx.decl.is_async {
      let result = set_up_async_stub_fn
        .call(scope, undefined.into(), &[key.into(), op_fn.into()])
        .unwrap();
      op_fn = result.try_into().unwrap()
    }

    deno_core_ops_obj.set(scope, key.into(), op_fn.into());
  }
}

fn op_ctx_function<'s>(
  scope: &mut v8::HandleScope<'s>,
  op_ctx: &OpCtx,
) -> v8::Local<'s, v8::Function> {
  let op_ctx_ptr = op_ctx as *const OpCtx as *const c_void;
  let external = v8::External::new(scope, op_ctx_ptr as *mut c_void);
  let v8name =
    v8_static_strings::new_from_static_str(scope, op_ctx.decl.name.as_bytes());

  let (slow_fn, fast_fn) = if op_ctx.metrics_enabled() {
    (
      op_ctx.decl.slow_fn_with_metrics,
      op_ctx.decl.fast_fn_with_metrics,
    )
  } else {
    (op_ctx.decl.slow_fn, op_ctx.decl.fast_fn)
  };

  let builder: v8::FunctionBuilder<v8::FunctionTemplate> =
    v8::FunctionTemplate::builder_raw(slow_fn)
      .data(external.into())
      .length(op_ctx.decl.arg_count as i32);

  let template = if let Some(fast_function) = &fast_fn {
    builder.build_fast(
      scope,
      fast_function,
      Some(op_ctx.fast_fn_c_info.unwrap().as_ptr()),
      None,
      None,
    )
  } else {
    builder.build(scope)
  };

  let v8fn = template.get_function(scope).unwrap();
  v8fn.set_name(v8name);
  v8fn
}

pub extern "C" fn wasm_async_resolve_promise_callback(
  _isolate: *mut v8::Isolate,
  context: v8::Local<v8::Context>,
  resolver: v8::Local<v8::PromiseResolver>,
  compilation_result: v8::Local<v8::Value>,
  success: v8::WasmAsyncSuccess,
) {
  // SAFETY: `CallbackScope` can be safely constructed from `Local<Context>`
  let scope = &mut unsafe { v8::CallbackScope::new(context) };
  if success == v8::WasmAsyncSuccess::Success {
    resolver.resolve(scope, compilation_result).unwrap();
  } else {
    resolver.reject(scope, compilation_result).unwrap();
  }
}

pub fn host_import_module_dynamically_callback<'s>(
  scope: &mut v8::HandleScope<'s>,
  _host_defined_options: v8::Local<'s, v8::Data>,
  resource_name: v8::Local<'s, v8::Value>,
  specifier: v8::Local<'s, v8::String>,
  import_attributes: v8::Local<'s, v8::FixedArray>,
) -> Option<v8::Local<'s, v8::Promise>> {
  // NOTE(bartlomieju): will crash for non-UTF-8 specifier
  let specifier_str = specifier
    .to_string(scope)
    .unwrap()
    .to_rust_string_lossy(scope);
  let referrer_name_str = resource_name
    .to_string(scope)
    .unwrap()
    .to_rust_string_lossy(scope);

  let resolver = v8::PromiseResolver::new(scope).unwrap();
  let promise = resolver.get_promise(scope);

  let assertions = parse_import_attributes(
    scope,
    import_attributes,
    ImportAttributesKind::DynamicImport,
  );

  {
    let tc_scope = &mut v8::TryCatch::new(scope);
    {
      let state = JsRuntime::state_from(tc_scope);
      if let Some(validate_import_attributes_cb) =
        &state.validate_import_attributes_cb
      {
        (validate_import_attributes_cb)(tc_scope, &assertions);
      }
    }

    if tc_scope.has_caught() {
      let e = tc_scope.exception().unwrap();
      resolver.reject(tc_scope, e);
    }
  }
  let requested_module_type =
    get_requested_module_type_from_attributes(&assertions);

  let resolver_handle = v8::Global::new(scope, resolver);
  {
    let state = JsRuntime::state_from(scope);
    let module_map_rc = JsRealm::module_map_from(scope);

    debug!(
      "dyn_import specifier {} referrer {} ",
      specifier_str, referrer_name_str
    );
    ModuleMap::load_dynamic_import(
      module_map_rc,
      &specifier_str,
      &referrer_name_str,
      requested_module_type,
      resolver_handle,
    );
    state.notify_new_dynamic_import();
  }
  // Map errors from module resolution (not JS errors from module execution) to
  // ones rethrown from this scope, so they include the call stack of the
  // dynamic import site. Error objects without any stack frames are assumed to
  // be module resolution errors, other exception values are left as they are.
  let builder = v8::FunctionBuilder::new(catch_dynamic_import_promise_error);

  let map_err =
    v8::FunctionBuilder::<v8::Function>::build(builder, scope).unwrap();

  let promise = promise.catch(scope, map_err).unwrap();

  Some(promise)
}

pub extern "C" fn host_initialize_import_meta_object_callback(
  context: v8::Local<v8::Context>,
  module: v8::Local<v8::Module>,
  meta: v8::Local<v8::Object>,
) {
  // SAFETY: `CallbackScope` can be safely constructed from `Local<Context>`
  let scope = &mut unsafe { v8::CallbackScope::new(context) };
  let module_map = JsRealm::module_map_from(scope);
  let state = JsRealm::state_from_scope(scope);

  let module_global = v8::Global::new(scope, module);
  let name = module_map
    .get_name_by_module(&module_global)
    .expect("Module not found");
  let module_type = module_map
    .get_type_by_module(&module_global)
    .expect("Module not found");

  let url_key = v8_static_strings::new(scope, &v8_static_strings::URL);
  let url_val = v8::String::new(scope, &name).unwrap();
  meta.create_data_property(scope, url_key.into(), url_val.into());

  let main_key = v8_static_strings::new(scope, &v8_static_strings::MAIN);
  let main = module_map.is_main_module(&module_global);
  let main_val = v8::Boolean::new(scope, main);
  meta.create_data_property(scope, main_key.into(), main_val.into());

  // Add special method that allows WASM module to instantiate themselves.
  if module_type == ModuleType::Wasm {
    let wasm_instantiate_key =
      v8_static_strings::new(scope, &v8_static_strings::WASM_INSTANTIATE);
    let f = state.wasm_instantiate_fn.borrow().as_ref().unwrap().clone();
    let wasm_instantiate_val = v8::Local::new(scope, &*f);
    meta.create_data_property(
      scope,
      wasm_instantiate_key.into(),
      wasm_instantiate_val.into(),
    );
  }

  let builder =
    v8::FunctionBuilder::new(import_meta_resolve).data(url_val.into());
  let val = v8::FunctionBuilder::<v8::Function>::build(builder, scope).unwrap();
  let resolve_key = v8_static_strings::new(scope, &v8_static_strings::RESOLVE);
  meta.set(scope, resolve_key.into(), val.into());

  maybe_add_import_meta_filename_dirname(scope, meta, &name);
}

fn maybe_add_import_meta_filename_dirname(
  scope: &mut v8::HandleScope,
  meta: v8::Local<v8::Object>,
  name: &str,
) {
  // For `file:` URL we provide additional `filename` and `dirname` values
  let Ok(name_url) = Url::parse(name) else {
    return;
  };

  if name_url.scheme() != "file" {
    return;
  }

  // If something goes wrong acquiring a filepath, let skip instead of crashing
  // (mostly concerned about file paths on Windows).
  let Ok(file_path) = name_url.to_file_path() else {
    return;
  };

  // Use display() here so that Rust takes care of proper forward/backward slash
  // formatting depending on the OS.
  let escaped_filename = file_path.display().to_string();
  let Some(filename_val) = v8::String::new(scope, &escaped_filename) else {
    return;
  };
  let filename_key =
    v8_static_strings::new(scope, &v8_static_strings::FILENAME);
  meta.create_data_property(scope, filename_key.into(), filename_val.into());

  let dir_path = file_path
    .parent()
    .map(|p| p.to_owned())
    .unwrap_or_else(|| PathBuf::from("/"));
  let escaped_dirname = dir_path.display().to_string();
  let Some(dirname_val) = v8::String::new(scope, &escaped_dirname) else {
    return;
  };
  let dirname_key = v8_static_strings::new(scope, &v8_static_strings::DIRNAME);
  meta.create_data_property(scope, dirname_key.into(), dirname_val.into());
}

fn import_meta_resolve(
  scope: &mut v8::HandleScope,
  args: v8::FunctionCallbackArguments,
  mut rv: v8::ReturnValue,
) {
  if args.length() > 1 {
    return throw_type_error(scope, "Invalid arguments");
  }

  let maybe_arg_str = args.get(0).to_string(scope);
  if maybe_arg_str.is_none() {
    return throw_type_error(scope, "Invalid arguments");
  }
  let specifier = maybe_arg_str.unwrap();
  let referrer = {
    let url_prop = args.data();
    url_prop.to_rust_string_lossy(scope)
  };
  let module_map_rc = JsRealm::module_map_from(scope);
  let loader = module_map_rc.loader.clone();
  let specifier_str = specifier.to_rust_string_lossy(scope);

  let import_meta_resolve_result = {
    let loader = loader.borrow();
    let loader = loader.as_ref();
    (module_map_rc.import_meta_resolve_cb)(loader, specifier_str, referrer)
  };

  match import_meta_resolve_result {
    Ok(resolved) => {
      let resolved_val = serde_v8::to_v8(scope, resolved.as_str()).unwrap();
      rv.set(resolved_val);
    }
    Err(err) => {
      throw_type_error(scope, &err.to_string());
    }
  };
}

fn empty_fn(
  _scope: &mut v8::HandleScope,
  _args: v8::FunctionCallbackArguments,
  _rv: v8::ReturnValue,
) {
  //Do Nothing
}

pub(crate) fn op_disabled_fn(
  scope: &mut v8::HandleScope,
  _args: v8::FunctionCallbackArguments,
  _rv: v8::ReturnValue,
) {
  let message = v8::String::new(scope, "op is disabled").unwrap();
  let exception = v8::Exception::error(scope, message);
  scope.throw_exception(exception);
}

//It creates a reference to an empty function which can be mantained after the snapshots
pub fn create_empty_fn<'s>(
  scope: &mut v8::HandleScope<'s>,
) -> Option<v8::Local<'s, v8::Function>> {
  let empty_fn = v8::FunctionTemplate::new(scope, empty_fn);
  empty_fn.get_function(scope)
}

fn catch_dynamic_import_promise_error(
  scope: &mut v8::HandleScope,
  args: v8::FunctionCallbackArguments,
  _rv: v8::ReturnValue,
) {
  let arg = args.get(0);
  if is_instance_of_error(scope, arg) {
    let e: crate::error::NativeJsError = serde_v8::from_v8(scope, arg).unwrap();
    let name = e.name.unwrap_or_else(|| "Error".to_string());
    if !has_call_site(scope, arg) {
      let msg = v8::Exception::create_message(scope, arg);
      let arg: v8::Local<v8::Object> = arg.try_into().unwrap();
      let message_key =
        v8_static_strings::new(scope, &v8_static_strings::MESSAGE);
      let message = arg.get(scope, message_key.into()).unwrap();
      let mut message: v8::Local<v8::String> = message.try_into().unwrap();
      if let Some(stack_frame) = JsStackFrame::from_v8_message(scope, msg) {
        if let Some(location) = stack_frame.maybe_format_location() {
          let str =
            format!("{} at {location}", message.to_rust_string_lossy(scope));
          message = v8::String::new(scope, &str).unwrap();
        }
      }
      let exception = match name.as_str() {
        "RangeError" => v8::Exception::range_error(scope, message),
        "TypeError" => v8::Exception::type_error(scope, message),
        "SyntaxError" => v8::Exception::syntax_error(scope, message),
        "ReferenceError" => v8::Exception::reference_error(scope, message),
        _ => v8::Exception::error(scope, message),
      };
      let code_key = v8_static_strings::new(scope, &v8_static_strings::CODE);
      let code_value =
        v8_static_strings::new(scope, &v8_static_strings::ERR_MODULE_NOT_FOUND);
      let exception_obj = exception.to_object(scope).unwrap();
      exception_obj.set(scope, code_key.into(), code_value.into());
      scope.throw_exception(exception);
      return;
    }
  }
  scope.throw_exception(arg);
}

pub extern "C" fn promise_reject_callback(message: v8::PromiseRejectMessage) {
  // SAFETY: `CallbackScope` can be safely constructed from `&PromiseRejectMessage`
  let scope = &mut unsafe { v8::CallbackScope::new(&message) };

  let exception_state = JsRealm::exception_state_from_scope(scope);
  exception_state.track_promise_rejection(
    scope,
    message.get_promise(),
    message.get_event(),
    message.get_value(),
  );
}

/// This binding should be used if there's a custom console implementation
/// available. Using it will make sure that proper stack frames are displayed
/// in the inspector console.
///
/// Each method on console object should be bound to this function, eg:
/// ```ignore
/// function wrapConsole(consoleFromDeno, consoleFromV8) {
///   const callConsole = core.callConsole;
///
///   for (const key of Object.keys(consoleFromV8)) {
///     if (consoleFromDeno.hasOwnProperty(key)) {
///       consoleFromDeno[key] = callConsole.bind(
///         consoleFromDeno,
///         consoleFromV8[key],
///         consoleFromDeno[key],
///       );
///     }
///   }
/// }
/// ```
///
/// Inspired by:
/// https://github.com/nodejs/node/blob/1317252dfe8824fd9cfee125d2aaa94004db2f3b/src/inspector_js_api.cc#L194-L222
fn call_console(
  scope: &mut v8::HandleScope,
  args: v8::FunctionCallbackArguments,
  _rv: v8::ReturnValue,
) {
  if args.length() < 2
    || !args.get(0).is_function()
    || !args.get(1).is_function()
  {
    return throw_type_error(scope, "Invalid arguments");
  }

  let mut call_args = vec![];
  for i in 2..args.length() {
    call_args.push(args.get(i));
  }

  let receiver = args.this();
  let inspector_console_method =
    v8::Local::<v8::Function>::try_from(args.get(0)).unwrap();
  let deno_console_method =
    v8::Local::<v8::Function>::try_from(args.get(1)).unwrap();

  inspector_console_method.call(scope, receiver.into(), &call_args);
  deno_console_method.call(scope, receiver.into(), &call_args);
}

/// Wrap a promise with `then` handlers allowing us to watch the resolution progress from a Rust closure.
/// This has a side-effect of preventing unhandled rejection handlers from triggering. If that is
/// desired, the final handler may choose to rethrow the exception.
pub(crate) fn watch_promise<'s, F>(
  scope: &mut v8::HandleScope<'s>,
  promise: v8::Local<'s, v8::Promise>,
  f: F,
) -> Option<v8::Local<'s, v8::Promise>>
where
  F: FnOnce(
      &mut v8::HandleScope,
      v8::ReturnValue,
      Result<v8::Local<v8::Value>, v8::Local<v8::Value>>,
    ) + 'static,
{
  let external =
    v8::External::new(scope, Box::into_raw(Box::new(Some(f))) as _);

  fn get_handler<F>(external: v8::Local<v8::External>) -> F {
    unsafe { Box::<Option<F>>::from_raw(external.value() as _) }
      .take()
      .unwrap()
  }

  let on_fulfilled = v8::Function::builder(
    |scope: &mut v8::HandleScope,
     args: v8::FunctionCallbackArguments,
     rv: v8::ReturnValue| {
      let data = v8::Local::<v8::External>::try_from(args.data()).unwrap();
      let f = get_handler::<F>(data);
      f(scope, rv, Ok(args.get(0)));
    },
  )
  .data(external.into())
  .build(scope);

  let on_rejected = v8::Function::builder(
    |scope: &mut v8::HandleScope,
     args: v8::FunctionCallbackArguments,
     rv: v8::ReturnValue| {
      let data = v8::Local::<v8::External>::try_from(args.data()).unwrap();
      let f = get_handler::<F>(data);
      f(scope, rv, Err(args.get(0)));
    },
  )
  .data(external.into())
  .build(scope);

  // function builders will return None if the runtime is shutting down
  let (Some(on_fulfilled), Some(on_rejected)) = (on_fulfilled, on_rejected)
  else {
    _ = get_handler::<F>(external);
    return None;
  };

  // then2 will return None if the runtime is shutting down
  let Some(promise) = promise.then2(scope, on_fulfilled, on_rejected) else {
    _ = get_handler::<F>(external);
    return None;
  };

  Some(promise)
}

/// This function generates a list of tuples, that are a mapping of `<op_name>`
/// to a JavaScript function that executes and op.
pub fn create_exports_for_ops_virtual_module<'s>(
  op_ctxs: &[OpCtx],
  scope: &mut v8::HandleScope<'s>,
  global: v8::Local<v8::Object>,
) -> Vec<(FastString, v8::Local<'s, v8::Value>)> {
  let mut exports = Vec::with_capacity(op_ctxs.len());

  let deno_obj = get(scope, global, &v8_static_strings::DENO, "Deno");
  let deno_core_obj =
    get(scope, deno_obj, &v8_static_strings::CORE, "Deno.core");
  let set_up_async_stub_fn: v8::Local<v8::Function> = get(
    scope,
    deno_core_obj,
    &v8_static_strings::SET_UP_ASYNC_STUB,
    "Deno.core.setUpAsyncStub",
  );

  let undefined = v8::undefined(scope);

  for op_ctx in op_ctxs {
    let name = v8_static_strings::new_from_static_str(
      scope,
      op_ctx.decl.name.as_bytes(),
    );
    let mut op_fn = op_ctx_function(scope, op_ctx);

    // For async ops we need to set them up, by calling `Deno.core.setUpAsyncStub` -
    // this call will generate an optimized function that binds to the provided
    // op, while keeping track of promises and error remapping.
    if op_ctx.decl.is_async {
      let result = set_up_async_stub_fn
        .call(scope, undefined.into(), &[name.into(), op_fn.into()])
        .unwrap();
      op_fn = result.try_into().unwrap()
    }

    exports.push((FastString::StaticAscii(op_ctx.decl.name), op_fn.into()));
  }

  exports
}
