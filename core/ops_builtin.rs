// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use crate::error::bad_resource;
use crate::error::format_file_name;
use crate::io::BufMutViewWhole;
use crate::io::BufView;
use crate::op2;
use crate::ops_builtin_types;
use crate::ops_builtin_v8;
use crate::JsBuffer;
use crate::OpState;
use crate::Resource;
use crate::ResourceId;
use anyhow::Error;
use bytes::BytesMut;
use std::cell::RefCell;
use std::io::stderr;
use std::io::stdout;
use std::io::Write;
use std::rc::Rc;

deno_core::extension!(
  core,
  ops = [
    op_close,
    op_try_close,
    op_print,
    op_resources,
    op_wasm_streaming_feed,
    op_wasm_streaming_set_url,
    op_void_sync,
    op_error_async,
    op_error_async_deferred,
    op_void_async,
    op_void_async_deferred,
    op_add,
    op_add_async,
    op_read,
    op_read_all,
    op_write,
    op_read_sync,
    op_write_sync,
    op_write_all,
    op_shutdown,
    op_format_file_name,
    op_str_byte_length,
    op_panic,
    ops_builtin_types::op_is_any_array_buffer,
    ops_builtin_types::op_is_arguments_object,
    ops_builtin_types::op_is_array_buffer,
    ops_builtin_types::op_is_array_buffer_view,
    ops_builtin_types::op_is_async_function,
    ops_builtin_types::op_is_big_int_object,
    ops_builtin_types::op_is_boolean_object,
    ops_builtin_types::op_is_boxed_primitive,
    ops_builtin_types::op_is_data_view,
    ops_builtin_types::op_is_date,
    ops_builtin_types::op_is_generator_function,
    ops_builtin_types::op_is_generator_object,
    ops_builtin_types::op_is_map,
    ops_builtin_types::op_is_map_iterator,
    ops_builtin_types::op_is_module_namespace_object,
    ops_builtin_types::op_is_native_error,
    ops_builtin_types::op_is_number_object,
    ops_builtin_types::op_is_promise,
    ops_builtin_types::op_is_proxy,
    ops_builtin_types::op_is_reg_exp,
    ops_builtin_types::op_is_set,
    ops_builtin_types::op_is_set_iterator,
    ops_builtin_types::op_is_shared_array_buffer,
    ops_builtin_types::op_is_string_object,
    ops_builtin_types::op_is_symbol_object,
    ops_builtin_types::op_is_typed_array,
    ops_builtin_types::op_is_weak_map,
    ops_builtin_types::op_is_weak_set,
    ops_builtin_v8::op_set_handled_promise_rejection_handler,
    ops_builtin_v8::op_timer_queue,
    ops_builtin_v8::op_timer_cancel,
    ops_builtin_v8::op_timer_ref,
    ops_builtin_v8::op_timer_unref,
    ops_builtin_v8::op_ref_op,
    ops_builtin_v8::op_unref_op,
    ops_builtin_v8::op_lazy_load_esm,
    ops_builtin_v8::op_run_microtasks,
    ops_builtin_v8::op_has_tick_scheduled,
    ops_builtin_v8::op_set_has_tick_scheduled,
    ops_builtin_v8::op_eval_context,
    ops_builtin_v8::op_queue_microtask,
    ops_builtin_v8::op_encode,
    ops_builtin_v8::op_decode,
    ops_builtin_v8::op_serialize,
    ops_builtin_v8::op_deserialize,
    ops_builtin_v8::op_set_promise_hooks,
    ops_builtin_v8::op_get_promise_details,
    ops_builtin_v8::op_get_proxy_details,
    ops_builtin_v8::op_get_non_index_property_names,
    ops_builtin_v8::op_get_constructor_name,
    ops_builtin_v8::op_memory_usage,
    ops_builtin_v8::op_set_wasm_streaming_callback,
    ops_builtin_v8::op_abort_wasm_streaming,
    ops_builtin_v8::op_destructure_error,
    ops_builtin_v8::op_dispatch_exception,
    ops_builtin_v8::op_op_names,
    ops_builtin_v8::op_apply_source_map,
    ops_builtin_v8::op_apply_source_map_filename,
    ops_builtin_v8::op_current_user_call_site,
    ops_builtin_v8::op_set_format_exception_callback,
    ops_builtin_v8::op_event_loop_has_more_work,
    ops_builtin_v8::op_arraybuffer_was_detached,
  ],
);

#[op2(fast)]
pub fn op_panic(#[string] message: String) {
  eprintln!("JS PANIC: {}", message);
  panic!("JS PANIC: {}", message);
}

/// Return map of resources with id as key
/// and string representation as value.
#[op2]
#[serde]
pub fn op_resources(state: &mut OpState) -> Vec<(ResourceId, String)> {
  state
    .resource_table
    .names()
    .map(|(rid, name)| (rid, name.to_string()))
    .collect()
}

#[op2(fast)]
fn op_add(a: i32, b: i32) -> i32 {
  a + b
}

#[op2(async)]
pub async fn op_add_async(a: i32, b: i32) -> i32 {
  a + b
}

#[op2(fast)]
pub fn op_void_sync() {}

#[op2(async)]
pub async fn op_void_async() {}

#[op2(async)]
pub async fn op_error_async() -> Result<(), Error> {
  Err(Error::msg("error"))
}

#[op2(async(deferred), fast)]
pub async fn op_error_async_deferred() -> Result<(), Error> {
  Err(Error::msg("error"))
}

#[op2(async(deferred), fast)]
pub async fn op_void_async_deferred() {}

/// Remove a resource from the resource table.
#[op2(fast)]
pub fn op_close(
  state: Rc<RefCell<OpState>>,
  #[smi] rid: ResourceId,
) -> Result<(), Error> {
  let resource = state.borrow_mut().resource_table.take_any(rid)?;
  Rc::try_unwrap(resource)
    .map_err(|_| bad_resource("resource locked"))?
    .close();
  Ok(())
}

/// Try to remove a resource from the resource table. If there is no resource
/// with the specified `rid`, this is a no-op.
#[op2(fast)]
pub fn op_try_close(state: Rc<RefCell<OpState>>, #[smi] rid: ResourceId) {
  if let Ok(resource) = state.borrow_mut().resource_table.take_any(rid) {
    if let Ok(resource) = Rc::try_unwrap(resource) {
      resource.close();
    }
  }
}

/// Builtin utility to print to stdout/stderr
#[op2(fast)]
pub fn op_print(#[string] msg: &str, is_err: bool) -> Result<(), Error> {
  if is_err {
    stderr().write_all(msg.as_bytes())?;
    stderr().flush().unwrap();
  } else {
    stdout().write_all(msg.as_bytes())?;
    stdout().flush().unwrap();
  }
  Ok(())
}

pub struct WasmStreamingResource(pub(crate) RefCell<v8::WasmStreaming>);

impl Resource for WasmStreamingResource {
  fn close(self) {
    self.0.into_inner().finish();
  }
}

/// Feed bytes to WasmStreamingResource.
#[op2(fast)]
pub fn op_wasm_streaming_feed(
  state: Rc<RefCell<OpState>>,
  #[smi] rid: ResourceId,
  #[buffer] bytes: &[u8],
) -> Result<(), Error> {
  let wasm_streaming = state
    .borrow_mut()
    .resource_table
    .get::<WasmStreamingResource>(rid)?;

  wasm_streaming.0.borrow_mut().on_bytes_received(bytes);

  Ok(())
}

#[op2(fast)]
pub fn op_wasm_streaming_set_url(
  state: &mut OpState,
  #[smi] rid: ResourceId,
  #[string] url: &str,
) -> Result<(), Error> {
  let wasm_streaming =
    state.resource_table.get::<WasmStreamingResource>(rid)?;

  wasm_streaming.0.borrow_mut().set_url(url);

  Ok(())
}

#[op2(async)]
async fn op_read(
  state: Rc<RefCell<OpState>>,
  #[smi] rid: ResourceId,
  #[buffer] buf: JsBuffer,
) -> Result<u32, Error> {
  let resource = state.borrow().resource_table.get_any(rid)?;
  let mut view = BufMutViewWhole::from(buf);
  resource.read(&mut view).await.map(|n| n as u32)
}

#[op2(async)]
#[buffer]
async fn op_read_all(
  state: Rc<RefCell<OpState>>,
  #[smi] rid: ResourceId,
) -> Result<BytesMut, Error> {
  let resource = state.borrow().resource_table.get_any(rid)?;
  resource.read_all().await
}

#[op2(async)]
async fn op_write(
  state: Rc<RefCell<OpState>>,
  #[smi] rid: ResourceId,
  #[buffer] buf: JsBuffer,
) -> Result<u32, Error> {
  let resource = state.borrow().resource_table.get_any(rid)?;
  let mut view = BufView::from(buf);
  resource.write(&mut view).await.map(|n| n as u32)
}

#[op2]
fn op_read_sync(
  state: Rc<RefCell<OpState>>,
  #[smi] rid: ResourceId,
  #[buffer] buf: JsBuffer,
) -> Result<u32, Error> {
  let resource = state.borrow_mut().resource_table.get_any(rid)?;
  resource.read_sync(buf.into()).map(|n| n as u32)
}

#[op2]
fn op_write_sync(
  state: Rc<RefCell<OpState>>,
  #[smi] rid: ResourceId,
  #[buffer] buf: JsBuffer,
) -> Result<u32, Error> {
  let resource = state.borrow_mut().resource_table.get_any(rid)?;
  let nwritten = resource.write_sync(buf.into())?;
  Ok(nwritten as u32)
}

#[op2(async)]
async fn op_write_all(
  state: Rc<RefCell<OpState>>,
  #[smi] rid: ResourceId,
  #[buffer] buf: JsBuffer,
) -> Result<(), Error> {
  let resource = state.borrow().resource_table.get_any(rid)?;
  resource.write_all(buf.into()).await?;
  Ok(())
}

#[op2(async)]
async fn op_shutdown(
  state: Rc<RefCell<OpState>>,
  #[smi] rid: ResourceId,
) -> Result<(), Error> {
  let resource = state.borrow().resource_table.get_any(rid)?;
  resource.shutdown().await
}

#[op2]
#[string]
fn op_format_file_name(#[string] file_name: &str) -> String {
  format_file_name(file_name)
}

#[op2]
fn op_str_byte_length(
  scope: &mut v8::HandleScope,
  value: v8::Local<v8::Value>,
) -> u32 {
  if let Ok(string) = v8::Local::<v8::String>::try_from(value) {
    string.utf8_length(scope) as u32
  } else {
    0
  }
}
