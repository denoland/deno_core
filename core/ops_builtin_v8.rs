// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use crate::error::custom_error;
use crate::error::is_instance_of_error;
use crate::error::range_error;
use crate::error::type_error;
use crate::error::JsError;
use crate::op2;
use crate::ops_builtin::WasmStreamingResource;
use crate::resolve_url;
use crate::runtime::script_origin;
use crate::runtime::JsRuntimeState;
use crate::source_map::apply_source_map;
use crate::source_map::SourceMapApplication;
use crate::JsBuffer;
use crate::JsRealm;
use crate::JsRuntime;
use anyhow::Error;
use serde::Deserialize;
use serde::Serialize;
use std::cell::RefCell;
use std::rc::Rc;
use v8::ValueDeserializerHelper;
use v8::ValueSerializerHelper;

#[op2]
pub fn op_ref_op(scope: &mut v8::HandleScope, promise_id: i32) {
  let context_state = JsRealm::state_from_scope(scope);
  context_state.borrow_mut().unrefed_ops.remove(&promise_id);
}

#[op2]
pub fn op_unref_op(scope: &mut v8::HandleScope, promise_id: i32) {
  let context_state = JsRealm::state_from_scope(scope);
  context_state.borrow_mut().unrefed_ops.insert(promise_id);
}

const SPECIFIER: &str = "foobar";
const SOURCE_CODE: &str = "Deno.core.print('foo\\n')";

#[op2]
#[global]
pub fn op_lazy_load_esm(scope: &mut v8::HandleScope) -> Result<v8::Global<v8::Value>, anyhow::Error> {
  let module_map_rc = JsRealm::module_map_from(scope);

  let mod_ns = futures::executor::block_on(async {
    // false for side module (not main module)
    let mod_id = module_map_rc
      .borrow_mut()
      .new_es_module(scope, false, SPECIFIER.to_string().into(), SOURCE_CODE.to_string().into(), false)
      .map_err(|e| match e {
        crate::modules::ModuleError::Exception(exception) => {
          let exception = v8::Local::new(scope, exception);
          crate::error::exception_to_err_result::<()>(scope, exception, false).unwrap_err()
        }
        crate::modules::ModuleError::Other(error) => error,
      })?;

    module_map_rc.borrow_mut().instantiate_module(scope, mod_id).unwrap();

    let module_handle = module_map_rc.borrow().get_handle(mod_id).unwrap();
    let module_local = v8::Local::new(scope, module_handle);

    let status = module_local.get_status();
    assert_eq!(status, v8::ModuleStatus::Instantiated);

    let _ = module_local.evaluate(scope);

    let status = module_local.get_status();
    assert_eq!(status, v8::ModuleStatus::Evaluated);

    let mod_ns = module_local.get_module_namespace();

    Ok::<_, anyhow::Error>(v8::Global::new(scope, mod_ns))
  });

  mod_ns
}

#[op2]
pub fn op_set_promise_reject_callback<'a>(
  scope: &mut v8::HandleScope<'a>,
  #[global] cb: v8::Global<v8::Function>,
) -> Option<v8::Local<'a, v8::Function>> {
  let context_state_rc = JsRealm::state_from_scope(scope);
  let old = context_state_rc
    .borrow_mut()
    .js_promise_reject_cb
    .replace(Rc::new(cb));
  old.map(|v| v8::Local::new(scope, &*v))
}

// We run in a `nofast` op here so we don't get put into a `DisallowJavascriptExecutionScope` and we're
// allowed to touch JS heap.
#[op2(nofast)]
pub fn op_queue_microtask(
  isolate: *mut v8::Isolate,
  cb: v8::Local<v8::Function>,
) {
  // SAFETY: we know v8 provides us a valid, non-null isolate pointer
  unsafe {
    isolate.as_mut().unwrap_unchecked().enqueue_microtask(cb);
  }
}

// We run in a `nofast` op here so we don't get put into a `DisallowJavascriptExecutionScope` and we're
// allowed to touch JS heap.
#[op2(nofast)]
pub fn op_run_microtasks(isolate: *mut v8::Isolate) {
  // SAFETY: we know v8 provides us with a valid, non-null isolate
  unsafe {
    isolate
      .as_mut()
      .unwrap_unchecked()
      .perform_microtask_checkpoint()
  };
}

#[op2(fast)]
pub fn op_has_tick_scheduled(state: &JsRuntimeState) -> bool {
  state.has_tick_scheduled
}

#[op2(fast)]
pub fn op_set_has_tick_scheduled(state: &mut JsRuntimeState, v: bool) {
  state.has_tick_scheduled = v;
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EvalContextError<'s> {
  thrown: serde_v8::Value<'s>,
  is_native_error: bool,
  is_compile_error: bool,
}

#[derive(Serialize)]
pub struct EvalContextResult<'s>(
  Option<serde_v8::Value<'s>>,
  Option<EvalContextError<'s>>,
);

#[op2]
#[serde]
pub fn op_eval_context<'a>(
  scope: &mut v8::HandleScope<'a>,
  source: v8::Local<'a, v8::Value>,
  #[string] specifier: String,
) -> Result<EvalContextResult<'a>, Error> {
  let tc_scope = &mut v8::TryCatch::new(scope);
  let source = v8::Local::<v8::String>::try_from(source)
    .map_err(|_| type_error("Invalid source"))?;
  let specifier = resolve_url(&specifier)?.to_string();
  let specifier = v8::String::new(tc_scope, &specifier).unwrap();
  let origin = script_origin(tc_scope, specifier);

  let script = match v8::Script::compile(tc_scope, source, Some(&origin)) {
    Some(s) => s,
    None => {
      assert!(tc_scope.has_caught());
      let exception = tc_scope.exception().unwrap();
      return Ok(EvalContextResult(
        None,
        Some(EvalContextError {
          thrown: exception.into(),
          is_native_error: is_instance_of_error(tc_scope, exception),
          is_compile_error: true,
        }),
      ));
    }
  };

  match script.run(tc_scope) {
    Some(result) => Ok(EvalContextResult(Some(result.into()), None)),
    None => {
      assert!(tc_scope.has_caught());
      let exception = tc_scope.exception().unwrap();
      Ok(EvalContextResult(
        None,
        Some(EvalContextError {
          thrown: exception.into(),
          is_native_error: is_instance_of_error(tc_scope, exception),
          is_compile_error: false,
        }),
      ))
    }
  }
}

#[op2]
pub fn op_create_host_object<'a>(
  scope: &mut v8::HandleScope<'a>,
) -> v8::Local<'a, v8::Object> {
  let template = v8::ObjectTemplate::new(scope);
  template.set_internal_field_count(1);
  template.new_instance(scope).unwrap()
}

#[op2]
pub fn op_encode<'a>(
  scope: &mut v8::HandleScope<'a>,
  text: v8::Local<'a, v8::Value>,
) -> Result<v8::Local<'a, v8::Uint8Array>, Error> {
  let text = v8::Local::<v8::String>::try_from(text)
    .map_err(|_| type_error("Invalid argument"))?;
  let text_str = serde_v8::to_utf8(text, scope);
  let bytes = text_str.into_bytes();
  let len = bytes.len();
  let backing_store =
    v8::ArrayBuffer::new_backing_store_from_vec(bytes).make_shared();
  let buffer = v8::ArrayBuffer::with_backing_store(scope, &backing_store);
  let u8array = v8::Uint8Array::new(scope, buffer, 0, len).unwrap();
  Ok(u8array)
}

#[op2]
pub fn op_decode<'a>(
  scope: &mut v8::HandleScope<'a>,
  #[buffer] zero_copy: &[u8],
) -> Result<v8::Local<'a, v8::String>, Error> {
  let buf = &zero_copy;

  // Strip BOM
  let buf =
    if buf.len() >= 3 && buf[0] == 0xef && buf[1] == 0xbb && buf[2] == 0xbf {
      &buf[3..]
    } else {
      buf
    };

  // If `String::new_from_utf8()` returns `None`, this means that the
  // length of the decoded string would be longer than what V8 can
  // handle. In this case we return `RangeError`.
  //
  // For more details see:
  // - https://encoding.spec.whatwg.org/#dom-textdecoder-decode
  // - https://github.com/denoland/deno/issues/6649
  // - https://github.com/v8/v8/blob/d68fb4733e39525f9ff0a9222107c02c28096e2a/include/v8.h#L3277-L3278
  match v8::String::new_from_utf8(scope, buf, v8::NewStringType::Normal) {
    Some(text) => Ok(text),
    None => Err(range_error("string too long")),
  }
}

struct SerializeDeserialize<'a> {
  host_objects: Option<v8::Local<'a, v8::Array>>,
  error_callback: Option<v8::Local<'a, v8::Function>>,
  for_storage: bool,
}

impl<'a> v8::ValueSerializerImpl for SerializeDeserialize<'a> {
  #[allow(unused_variables)]
  fn throw_data_clone_error<'s>(
    &mut self,
    scope: &mut v8::HandleScope<'s>,
    message: v8::Local<'s, v8::String>,
  ) {
    if let Some(cb) = self.error_callback {
      let scope = &mut v8::TryCatch::new(scope);
      let undefined = v8::undefined(scope).into();
      cb.call(scope, undefined, &[message.into()]);
      if scope.has_caught() || scope.has_terminated() {
        scope.rethrow();
        return;
      };
    }
    let error = v8::Exception::type_error(scope, message);
    scope.throw_exception(error);
  }

  fn get_shared_array_buffer_id<'s>(
    &mut self,
    scope: &mut v8::HandleScope<'s>,
    shared_array_buffer: v8::Local<'s, v8::SharedArrayBuffer>,
  ) -> Option<u32> {
    if self.for_storage {
      return None;
    }
    let state_rc = JsRuntime::state_from(scope);
    let state = state_rc.borrow_mut();
    if let Some(shared_array_buffer_store) = &state.shared_array_buffer_store {
      let backing_store = shared_array_buffer.get_backing_store();
      let id = shared_array_buffer_store.insert(backing_store);
      Some(id)
    } else {
      None
    }
  }

  fn get_wasm_module_transfer_id(
    &mut self,
    scope: &mut v8::HandleScope<'_>,
    module: v8::Local<v8::WasmModuleObject>,
  ) -> Option<u32> {
    if self.for_storage {
      let message = v8::String::new(scope, "Wasm modules cannot be stored")?;
      self.throw_data_clone_error(scope, message);
      return None;
    }
    let state_rc = JsRuntime::state_from(scope);
    let state = state_rc.borrow_mut();
    if let Some(compiled_wasm_module_store) = &state.compiled_wasm_module_store
    {
      let compiled_wasm_module = module.get_compiled_module();
      let id = compiled_wasm_module_store.insert(compiled_wasm_module);
      Some(id)
    } else {
      None
    }
  }

  fn write_host_object<'s>(
    &mut self,
    scope: &mut v8::HandleScope<'s>,
    object: v8::Local<'s, v8::Object>,
    value_serializer: &mut dyn v8::ValueSerializerHelper,
  ) -> Option<bool> {
    if let Some(host_objects) = self.host_objects {
      for i in 0..host_objects.length() {
        let value = host_objects.get_index(scope, i).unwrap();
        if value == object {
          value_serializer.write_uint32(i);
          return Some(true);
        }
      }
    }
    let message = v8::String::new(scope, "Unsupported object type").unwrap();
    self.throw_data_clone_error(scope, message);
    None
  }
}

impl<'a> v8::ValueDeserializerImpl for SerializeDeserialize<'a> {
  fn get_shared_array_buffer_from_id<'s>(
    &mut self,
    scope: &mut v8::HandleScope<'s>,
    transfer_id: u32,
  ) -> Option<v8::Local<'s, v8::SharedArrayBuffer>> {
    if self.for_storage {
      return None;
    }
    let state_rc = JsRuntime::state_from(scope);
    let state = state_rc.borrow_mut();
    if let Some(shared_array_buffer_store) = &state.shared_array_buffer_store {
      let backing_store = shared_array_buffer_store.take(transfer_id)?;
      let shared_array_buffer =
        v8::SharedArrayBuffer::with_backing_store(scope, &backing_store);
      Some(shared_array_buffer)
    } else {
      None
    }
  }

  fn get_wasm_module_from_id<'s>(
    &mut self,
    scope: &mut v8::HandleScope<'s>,
    clone_id: u32,
  ) -> Option<v8::Local<'s, v8::WasmModuleObject>> {
    if self.for_storage {
      return None;
    }
    let state_rc = JsRuntime::state_from(scope);
    let state = state_rc.borrow_mut();
    if let Some(compiled_wasm_module_store) = &state.compiled_wasm_module_store
    {
      let compiled_module = compiled_wasm_module_store.take(clone_id)?;
      v8::WasmModuleObject::from_compiled_module(scope, &compiled_module)
    } else {
      None
    }
  }

  fn read_host_object<'s>(
    &mut self,
    scope: &mut v8::HandleScope<'s>,
    value_deserializer: &mut dyn v8::ValueDeserializerHelper,
  ) -> Option<v8::Local<'s, v8::Object>> {
    if let Some(host_objects) = self.host_objects {
      let mut i = 0;
      if !value_deserializer.read_uint32(&mut i) {
        return None;
      }
      let maybe_value = host_objects.get_index(scope, i);
      if let Some(value) = maybe_value {
        return value.to_object(scope);
      }
    }

    let message: v8::Local<v8::String> =
      v8::String::new(scope, "Failed to deserialize host object").unwrap();
    let error = v8::Exception::error(scope, message);
    scope.throw_exception(error);
    None
  }
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SerializeDeserializeOptions<'a> {
  host_objects: Option<serde_v8::Value<'a>>,
  transferred_array_buffers: Option<serde_v8::Value<'a>>,
  #[serde(default)]
  for_storage: bool,
}

#[op2]
#[buffer]
pub fn op_serialize(
  scope: &mut v8::HandleScope,
  value: v8::Local<v8::Value>,
  #[serde] options: Option<SerializeDeserializeOptions>,
  error_callback: Option<v8::Local<v8::Value>>,
) -> Result<Vec<u8>, Error> {
  let options = options.unwrap_or_default();
  let error_callback = match error_callback {
    Some(cb) => Some(
      v8::Local::<v8::Function>::try_from(cb)
        .map_err(|_| type_error("Invalid error callback"))?,
    ),
    None => None,
  };
  let host_objects = match options.host_objects {
    Some(value) => Some(
      v8::Local::<v8::Array>::try_from(value.v8_value)
        .map_err(|_| type_error("hostObjects not an array"))?,
    ),
    None => None,
  };
  let transferred_array_buffers = match options.transferred_array_buffers {
    Some(value) => Some(
      v8::Local::<v8::Array>::try_from(value.v8_value)
        .map_err(|_| type_error("transferredArrayBuffers not an array"))?,
    ),
    None => None,
  };

  let serialize_deserialize = Box::new(SerializeDeserialize {
    host_objects,
    error_callback,
    for_storage: options.for_storage,
  });
  let mut value_serializer =
    v8::ValueSerializer::new(scope, serialize_deserialize);
  value_serializer.write_header();

  if let Some(transferred_array_buffers) = transferred_array_buffers {
    let state_rc = JsRuntime::state_from(scope);
    let state = state_rc.borrow_mut();
    for index in 0..transferred_array_buffers.length() {
      let i = v8::Number::new(scope, index as f64).into();
      let buf = transferred_array_buffers.get(scope, i).unwrap();
      let buf = v8::Local::<v8::ArrayBuffer>::try_from(buf).map_err(|_| {
        type_error("item in transferredArrayBuffers not an ArrayBuffer")
      })?;
      if let Some(shared_array_buffer_store) = &state.shared_array_buffer_store
      {
        if !buf.is_detachable() {
          return Err(type_error(
            "item in transferredArrayBuffers is not transferable",
          ));
        }

        if buf.was_detached() {
          return Err(custom_error(
            "DOMExceptionOperationError",
            format!("ArrayBuffer at index {index} is already detached"),
          ));
        }

        let backing_store = buf.get_backing_store();
        buf.detach(None);
        let id = shared_array_buffer_store.insert(backing_store);
        value_serializer.transfer_array_buffer(id, buf);
        let id = v8::Number::new(scope, id as f64).into();
        transferred_array_buffers.set(scope, i, id);
      }
    }
  }

  let scope = &mut v8::TryCatch::new(scope);
  let ret = value_serializer.write_value(scope.get_current_context(), value);
  if scope.has_caught() || scope.has_terminated() {
    scope.rethrow();
    // Dummy value, this result will be discarded because an error was thrown.
    Ok(vec![])
  } else if let Some(true) = ret {
    let vector = value_serializer.release();
    Ok(vector)
  } else {
    Err(type_error("Failed to serialize response"))
  }
}

#[op2]
pub fn op_deserialize<'a>(
  scope: &mut v8::HandleScope<'a>,
  #[buffer] zero_copy: JsBuffer,
  #[serde] options: Option<SerializeDeserializeOptions>,
) -> Result<v8::Local<'a, v8::Value>, Error> {
  let options = options.unwrap_or_default();
  let host_objects = match options.host_objects {
    Some(value) => Some(
      v8::Local::<v8::Array>::try_from(value.v8_value)
        .map_err(|_| type_error("hostObjects not an array"))?,
    ),
    None => None,
  };
  let transferred_array_buffers = match options.transferred_array_buffers {
    Some(value) => Some(
      v8::Local::<v8::Array>::try_from(value.v8_value)
        .map_err(|_| type_error("transferredArrayBuffers not an array"))?,
    ),
    None => None,
  };

  let serialize_deserialize = Box::new(SerializeDeserialize {
    host_objects,
    error_callback: None,
    for_storage: options.for_storage,
  });
  let mut value_deserializer =
    v8::ValueDeserializer::new(scope, serialize_deserialize, &zero_copy);
  let parsed_header = value_deserializer
    .read_header(scope.get_current_context())
    .unwrap_or_default();
  if !parsed_header {
    return Err(range_error("could not deserialize value"));
  }

  if let Some(transferred_array_buffers) = transferred_array_buffers {
    let state_rc = JsRuntime::state_from(scope);
    let state = state_rc.borrow_mut();
    if let Some(shared_array_buffer_store) = &state.shared_array_buffer_store {
      for i in 0..transferred_array_buffers.length() {
        let i = v8::Number::new(scope, i as f64).into();
        let id_val = transferred_array_buffers.get(scope, i).unwrap();
        let id = match id_val.number_value(scope) {
          Some(id) => id as u32,
          None => {
            return Err(type_error(
              "item in transferredArrayBuffers not number",
            ))
          }
        };
        if let Some(backing_store) = shared_array_buffer_store.take(id) {
          let array_buffer =
            v8::ArrayBuffer::with_backing_store(scope, &backing_store);
          value_deserializer.transfer_array_buffer(id, array_buffer);
          transferred_array_buffers.set(scope, i, array_buffer.into());
        } else {
          return Err(type_error(
            "transferred array buffer not present in shared_array_buffer_store",
          ));
        }
      }
    }
  }

  let value = value_deserializer.read_value(scope.get_current_context());
  match value {
    Some(deserialized) => Ok(deserialized),
    None => Err(range_error("could not deserialize value")),
  }
}

#[derive(Serialize)]
pub struct PromiseDetails<'s>(u32, Option<serde_v8::Value<'s>>);

#[op2]
#[serde]
pub fn op_get_promise_details<'a>(
  scope: &mut v8::HandleScope<'a>,
  promise: v8::Local<'a, v8::Promise>,
) -> Result<PromiseDetails<'a>, Error> {
  match promise.state() {
    v8::PromiseState::Pending => Ok(PromiseDetails(0, None)),
    v8::PromiseState::Fulfilled => {
      Ok(PromiseDetails(1, Some(promise.result(scope).into())))
    }
    v8::PromiseState::Rejected => {
      Ok(PromiseDetails(2, Some(promise.result(scope).into())))
    }
  }
}

#[op2]
pub fn op_set_promise_hooks(
  scope: &mut v8::HandleScope,
  init_hook: v8::Local<v8::Value>,
  before_hook: v8::Local<v8::Value>,
  after_hook: v8::Local<v8::Value>,
  resolve_hook: v8::Local<v8::Value>,
) -> Result<(), Error> {
  let v8_fns = [init_hook, before_hook, after_hook, resolve_hook]
    .into_iter()
    .enumerate()
    .filter(|(_, hook)| !hook.is_undefined())
    .try_fold([None; 4], |mut v8_fns, (i, hook)| {
      let v8_fn = v8::Local::<v8::Function>::try_from(hook)
        .map_err(|err| type_error(err.to_string()))?;
      v8_fns[i] = Some(v8_fn);
      Ok::<_, Error>(v8_fns)
    })?;

  scope.set_promise_hooks(
    v8_fns[0], // init
    v8_fns[1], // before
    v8_fns[2], // after
    v8_fns[3], // resolve
  );

  Ok(())
}

// Based on https://github.com/nodejs/node/blob/1e470510ff74391d7d4ec382909ea8960d2d2fbc/src/node_util.cc
// Copyright Joyent, Inc. and other Node contributors.
//
// Permission is hereby granted, free of charge, to any person obtaining a
// copy of this software and associated documentation files (the
// "Software"), to deal in the Software without restriction, including
// without limitation the rights to use, copy, modify, merge, publish,
// distribute, sublicense, and/or sell copies of the Software, and to permit
// persons to whom the Software is furnished to do so, subject to the
// following conditions:
//
// The above copyright notice and this permission notice shall be included
// in all copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS
// OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
// MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN
// NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM,
// DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR
// OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE
// USE OR OTHER DEALINGS IN THE SOFTWARE.

#[op2]
#[serde]
pub fn op_get_proxy_details<'a>(
  scope: &mut v8::HandleScope<'a>,
  proxy: v8::Local<'a, v8::Value>,
) -> Option<(serde_v8::Value<'a>, serde_v8::Value<'a>)> {
  let proxy = match v8::Local::<v8::Proxy>::try_from(proxy) {
    Ok(proxy) => proxy,
    Err(_) => return None,
  };
  let target = proxy.get_target(scope);
  let handler = proxy.get_handler(scope);
  Some((target.into(), handler.into()))
}

#[op2]
pub fn op_get_non_index_property_names<'a>(
  scope: &mut v8::HandleScope<'a>,
  obj: v8::Local<'a, v8::Value>,
  filter: u32,
) -> Option<v8::Local<'a, v8::Value>> {
  let obj = match v8::Local::<v8::Object>::try_from(obj) {
    Ok(proxy) => proxy,
    Err(_) => return None,
  };

  let mut property_filter = v8::PropertyFilter::ALL_PROPERTIES;
  if filter & 1 == 1 {
    property_filter = property_filter | v8::PropertyFilter::ONLY_WRITABLE
  }
  if filter & 2 == 2 {
    property_filter = property_filter | v8::PropertyFilter::ONLY_ENUMERABLE
  }
  if filter & 4 == 4 {
    property_filter = property_filter | v8::PropertyFilter::ONLY_CONFIGURABLE
  }
  if filter & 8 == 8 {
    property_filter = property_filter | v8::PropertyFilter::SKIP_STRINGS
  }
  if filter & 16 == 16 {
    property_filter = property_filter | v8::PropertyFilter::SKIP_SYMBOLS
  }

  let maybe_names = obj.get_property_names(
    scope,
    v8::GetPropertyNamesArgs {
      mode: v8::KeyCollectionMode::OwnOnly,
      property_filter,
      index_filter: v8::IndexFilter::SkipIndices,
      ..Default::default()
    },
  );

  maybe_names.map(|names| names.into())
}

#[op2]
#[string]
pub fn op_get_constructor_name(
  scope: &mut v8::HandleScope,
  obj: v8::Local<v8::Value>,
) -> Option<String> {
  let obj = match v8::Local::<v8::Object>::try_from(obj) {
    Ok(proxy) => proxy,
    Err(_) => return None,
  };

  let name = obj.get_constructor_name().to_rust_string_lossy(scope);
  Some(name)
}

// HeapStats stores values from a isolate.get_heap_statistics() call
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryUsage {
  physical_total: usize,
  heap_total: usize,
  heap_used: usize,
  external: usize,
  // TODO: track ArrayBuffers, would require using a custom allocator to track
  // but it's otherwise a subset of external so can be indirectly tracked
  // array_buffers: usize,
}

#[op2]
#[serde]
pub fn op_memory_usage(scope: &mut v8::HandleScope) -> MemoryUsage {
  let mut s = v8::HeapStatistics::default();
  scope.get_heap_statistics(&mut s);
  MemoryUsage {
    physical_total: s.total_physical_size(),
    heap_total: s.total_heap_size(),
    heap_used: s.used_heap_size(),
    external: s.external_memory(),
  }
}

#[op2]
pub fn op_set_wasm_streaming_callback(
  scope: &mut v8::HandleScope,
  #[global] cb: v8::Global<v8::Function>,
) -> Result<(), Error> {
  let context_state_rc = JsRealm::state_from_scope(scope);
  let mut context_state = context_state_rc.borrow_mut();
  // The callback to pass to the v8 API has to be a unit type, so it can't
  // borrow or move any local variables. Therefore, we're storing the JS
  // callback in a JsRuntimeState slot.
  if context_state.js_wasm_streaming_cb.is_some() {
    return Err(type_error("op_set_wasm_streaming_callback already called"));
  }
  context_state.js_wasm_streaming_cb = Some(Rc::new(cb));

  scope.set_wasm_streaming_callback(|scope, arg, wasm_streaming| {
    let (cb_handle, streaming_rid) = {
      let context_state_rc = JsRealm::state_from_scope(scope);
      let cb_handle = context_state_rc
        .borrow()
        .js_wasm_streaming_cb
        .as_ref()
        .unwrap()
        .clone();
      let state_rc = JsRuntime::state_from(scope);
      let streaming_rid = state_rc
        .borrow()
        .op_state
        .borrow_mut()
        .resource_table
        .add(WasmStreamingResource(RefCell::new(wasm_streaming)));
      (cb_handle, streaming_rid)
    };

    let undefined = v8::undefined(scope);
    let rid = serde_v8::to_v8(scope, streaming_rid).unwrap();
    cb_handle
      .open(scope)
      .call(scope, undefined.into(), &[arg, rid]);
  });
  Ok(())
}

#[allow(clippy::let_and_return)]
#[op2]
pub fn op_abort_wasm_streaming(
  scope: &mut v8::HandleScope,
  rid: u32,
  error: v8::Local<v8::Value>,
) -> Result<(), Error> {
  let wasm_streaming = {
    let state_rc = JsRuntime::state_from(scope);
    let state = state_rc.borrow();
    let wsr = state
      .op_state
      .borrow_mut()
      .resource_table
      .take::<WasmStreamingResource>(rid)?;
    wsr
  };

  // At this point there are no clones of Rc<WasmStreamingResource> on the
  // resource table, and no one should own a reference because we're never
  // cloning them. So we can be sure `wasm_streaming` is the only reference.
  if let Ok(wsr) = std::rc::Rc::try_unwrap(wasm_streaming) {
    // NOTE: v8::WasmStreaming::abort can't be called while `state` is borrowed;
    // see https://github.com/denoland/deno/issues/13917
    wsr.0.into_inner().abort(Some(error));
  } else {
    panic!("Couldn't consume WasmStreamingResource.");
  }
  Ok(())
}

#[op2]
#[serde]
pub fn op_destructure_error(
  scope: &mut v8::HandleScope,
  error: v8::Local<v8::Value>,
) -> JsError {
  JsError::from_v8_exception(scope, error)
}

/// Effectively throw an uncatchable error. This will terminate runtime
/// execution before any more JS code can run, except in the REPL where it
/// should just output the error to the console.
#[op2]
pub fn op_dispatch_exception(
  scope: &mut v8::HandleScope,
  exception: v8::Local<v8::Value>,
) {
  let state_rc = JsRuntime::state_from(scope);
  let mut state = state_rc.borrow_mut();
  if let Some(inspector) = &state.inspector {
    let inspector = inspector.borrow();
    inspector.exception_thrown(scope, exception, false);
    // This indicates that the op is being called from a REPL. Skip termination.
    if inspector.is_dispatching_message() {
      return;
    }
  }
  state.dispatched_exception = Some(v8::Global::new(scope, exception));
  scope.terminate_execution();
}

#[op2]
#[serde]
pub fn op_op_names(scope: &mut v8::HandleScope) -> Vec<String> {
  let state_rc = JsRealm::state_from_scope(scope);
  let state = state_rc.borrow();
  state
    .op_ctxs
    .iter()
    .map(|o| o.decl.name.to_string())
    .collect()
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Location {
  file_name: String,
  line_number: u32,
  column_number: u32,
}

fn write_line_and_col_to_ret_buf(
  ret_buf: &mut [u8],
  line_number: u32,
  column_number: u32,
) {
  ret_buf[0..4].copy_from_slice(&line_number.to_le_bytes());
  ret_buf[4..8].copy_from_slice(&column_number.to_le_bytes());
}

// Returns:
// 0: no source mapping performed, use original location
// 1: mapped line and column, but not file name. new line and column are in
//    ret_buf, use original file name.
// 2: mapped line, column, and file name. new line, column, and file name are in
//    ret_buf. retrieve file name by calling `op_apply_source_map_filename`
//    immediately after this op returns.
#[op2(fast)]
#[smi]
pub fn op_apply_source_map(
  state: &JsRuntimeState,
  #[string] file_name: &str,
  #[smi] line_number: u32,
  #[smi] column_number: u32,
  #[buffer] ret_buf: &mut [u8],
) -> Result<u8, Error> {
  if ret_buf.len() != 8 {
    return Err(type_error("retBuf must be 8 bytes"));
  }
  if let Some(source_map_getter) = state.source_map_getter.as_ref() {
    let mut cache = state.source_map_cache.borrow_mut();
    let application = apply_source_map(
      file_name,
      line_number,
      column_number,
      &mut cache,
      &***source_map_getter,
    );
    match application {
      SourceMapApplication::Unchanged => Ok(0),
      SourceMapApplication::LineAndColumn {
        line_number,
        column_number,
      } => {
        write_line_and_col_to_ret_buf(ret_buf, line_number, column_number);
        Ok(1)
      }
      SourceMapApplication::LineAndColumnAndFileName {
        line_number,
        column_number,
        file_name,
      } => {
        write_line_and_col_to_ret_buf(ret_buf, line_number, column_number);
        cache.stashed_file_name.replace(file_name);
        Ok(2)
      }
    }
  } else {
    Ok(0)
  }
}

// Call to retrieve the stashed file name from a previous call to
// `op_apply_source_map` that returned `2`.
#[op2]
#[string]
pub fn op_apply_source_map_filename(
  state: &JsRuntimeState,
) -> Result<String, Error> {
  state
    .source_map_cache
    .borrow_mut()
    .stashed_file_name
    .take()
    .ok_or_else(|| type_error("No stashed file name"))
}

#[op2]
#[string]
pub fn op_current_user_call_site(
  scope: &mut v8::HandleScope,
  js_runtime_state: &mut JsRuntimeState,
  #[buffer] ret_buf: &mut [u8],
) -> String {
  let stack_trace = v8::StackTrace::current_stack_trace(scope, 10).unwrap();
  let frame_count = stack_trace.get_frame_count();
  for i in 0..frame_count {
    let frame = stack_trace.get_frame(scope, i).unwrap();
    if !frame.is_user_javascript() {
      continue;
    }
    let file_name = frame
      .get_script_name(scope)
      .unwrap()
      .to_rust_string_lossy(scope);
    // TODO: this condition should be configurable. It's a CLI assumption.
    if (file_name.starts_with("ext:") || file_name.starts_with("node:"))
      && i != frame_count - 1
    {
      continue;
    }
    let line_number = frame.get_line_number() as u32;
    let column_number = frame.get_column() as u32;
    let application = if let Some(source_map_getter) =
      js_runtime_state.source_map_getter.as_ref()
    {
      let mut cache = js_runtime_state.source_map_cache.borrow_mut();
      apply_source_map(
        &file_name,
        line_number,
        column_number,
        &mut cache,
        &***source_map_getter,
      )
    } else {
      SourceMapApplication::Unchanged
    };

    match application {
      SourceMapApplication::Unchanged => {
        write_line_and_col_to_ret_buf(ret_buf, line_number, column_number);
        return file_name;
      }
      SourceMapApplication::LineAndColumn {
        line_number,
        column_number,
      } => {
        write_line_and_col_to_ret_buf(ret_buf, line_number, column_number);
        return file_name;
      }
      SourceMapApplication::LineAndColumnAndFileName {
        line_number,
        column_number,
        file_name,
      } => {
        write_line_and_col_to_ret_buf(ret_buf, line_number, column_number);
        return file_name;
      }
    }
  }

  unreachable!("No stack frames found on stack at all");
}

/// Set a callback which formats exception messages as stored in
/// `JsError::exception_message`. The callback is passed the error value and
/// should return a string or `null`. If no callback is set or the callback
/// returns `null`, the built-in default formatting will be used.
#[op2]
pub fn op_set_format_exception_callback<'a>(
  scope: &mut v8::HandleScope<'a>,
  #[global] cb: v8::Global<v8::Function>,
) -> Option<v8::Local<'a, v8::Value>> {
  let context_state_rc = JsRealm::state_from_scope(scope);
  let old = context_state_rc
    .borrow_mut()
    .js_format_exception_cb
    .replace(Rc::new(cb));
  let old = old.map(|v| v8::Local::new(scope, &*v));
  old.map(|func| func.into())
}

#[op2]
pub fn op_event_loop_has_more_work(scope: &mut v8::HandleScope) -> bool {
  JsRuntime::event_loop_pending_state_from_scope(scope).is_pending()
}

#[op2]
pub fn op_store_pending_promise_rejection(
  scope: &mut v8::HandleScope,
  #[global] promise: v8::Global<v8::Promise>,
  #[global] reason: v8::Global<v8::Value>,
) {
  let context_state_rc = JsRealm::state_from_scope(scope);
  let mut context_state = context_state_rc.borrow_mut();
  context_state
    .pending_promise_rejections
    .push_back((promise, reason));
}

#[op2]
pub fn op_remove_pending_promise_rejection(
  scope: &mut v8::HandleScope,
  #[global] promise: v8::Global<v8::Promise>,
) {
  let context_state_rc = JsRealm::state_from_scope(scope);
  let mut context_state = context_state_rc.borrow_mut();
  context_state
    .pending_promise_rejections
    .retain(|(key, _)| key != &promise);
}

#[op2]
pub fn op_has_pending_promise_rejection(
  scope: &mut v8::HandleScope,
  #[global] promise: v8::Global<v8::Promise>,
) -> bool {
  let context_state_rc = JsRealm::state_from_scope(scope);
  let context_state = context_state_rc.borrow();
  context_state
    .pending_promise_rejections
    .iter()
    .any(|(key, _)| key == &promise)
}

#[op2]
pub fn op_arraybuffer_was_detached(
  _scope: &mut v8::HandleScope,
  input: v8::Local<v8::Value>,
) -> Result<bool, Error> {
  let ab = v8::Local::<v8::ArrayBuffer>::try_from(input)?;
  Ok(ab.was_detached())
}
