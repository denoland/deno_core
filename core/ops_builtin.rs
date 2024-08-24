// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
use crate::error::format_file_name;
use crate::error::type_error;
use crate::io::AdaptiveBufferStrategy;
use crate::io::BufMutView;
use crate::io::BufView;
use crate::io::ResourceId;
use crate::op2;
use crate::ops_builtin_types;
use crate::ops_builtin_v8;
use crate::CancelHandle;
use crate::JsBuffer;
use crate::OpDecl;
use crate::OpState;
use crate::Resource;
use anyhow::Error;
use bytes::BytesMut;
use serde_v8::ByteString;
use std::cell::RefCell;
use std::io::stderr;
use std::io::stdout;
use std::io::Write;
use std::rc::Rc;

macro_rules! builtin_ops {
  ( $($op:ident $(:: $sub:ident)*),* ) => {
    pub const BUILTIN_OPS: &'static [OpDecl] = &[
      $( $op $(:: $sub) * () ),*
    ];
  }
}

builtin_ops! {
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
  op_write_type_error,
  op_shutdown,
  op_format_file_name,
  op_str_byte_length,
  op_panic,
  op_cancel_handle,
  op_encode_binary_string,
  op_is_terminal,
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
  ops_builtin_v8::op_add_main_module_handler,
  ops_builtin_v8::op_set_handled_promise_rejection_handler,
  ops_builtin_v8::op_timer_queue,
  ops_builtin_v8::op_timer_queue_system,
  ops_builtin_v8::op_timer_queue_immediate,
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
  ops_builtin_v8::op_create_inspector_session,
  ops_builtin_v8::op_create_inspector_session2,
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
  ops_builtin_v8::op_get_extras_binding_object,
  ops_builtin_v8::op_memory_usage,
  ops_builtin_v8::op_set_wasm_streaming_callback,
  ops_builtin_v8::op_abort_wasm_streaming,
  ops_builtin_v8::op_destructure_error,
  ops_builtin_v8::op_dispatch_exception,
  ops_builtin_v8::op_op_names,
  ops_builtin_v8::op_current_user_call_site,
  ops_builtin_v8::op_set_format_exception_callback,
  ops_builtin_v8::op_event_loop_has_more_work,
  ops_builtin_v8::op_leak_tracing_enable,
  ops_builtin_v8::op_leak_tracing_submit,
  ops_builtin_v8::op_leak_tracing_get_all,
  ops_builtin_v8::op_leak_tracing_get
}

#[op2(fast)]
pub fn op_panic(#[string] message: String) {
  #[allow(clippy::print_stderr)]
  {
    eprintln!("JS PANIC: {}", message);
  }
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

#[allow(clippy::unused_async)]
#[op2(async)]
pub async fn op_add_async(a: i32, b: i32) -> i32 {
  a + b
}

#[op2(fast)]
pub fn op_void_sync() {}

#[allow(clippy::unused_async)]
#[op2(async)]
pub async fn op_void_async() {}

#[allow(clippy::unused_async)]
#[op2(async)]
pub async fn op_error_async() -> Result<(), Error> {
  Err(Error::msg("error"))
}

#[allow(clippy::unused_async)]
#[op2(async(deferred), fast)]
pub async fn op_error_async_deferred() -> Result<(), Error> {
  Err(Error::msg("error"))
}

#[allow(clippy::unused_async)]
#[op2(async(deferred), fast)]
pub async fn op_void_async_deferred() {}

/// Remove a resource from the resource table.
#[op2(fast)]
pub fn op_close(
  state: Rc<RefCell<OpState>>,
  #[smi] rid: ResourceId,
) -> Result<(), Error> {
  let resource = state.borrow_mut().resource_table.take_any(rid)?;
  resource.close();
  Ok(())
}

/// Try to remove a resource from the resource table. If there is no resource
/// with the specified `rid`, this is a no-op.
#[op2(fast)]
pub fn op_try_close(state: Rc<RefCell<OpState>>, #[smi] rid: ResourceId) {
  if let Ok(resource) = state.borrow_mut().resource_table.take_any(rid) {
    resource.close();
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
  fn close(self: Rc<Self>) {
    // At this point there are no clones of Rc<WasmStreamingResource> on the
    // resource table, and no one should own a reference outside of the stack.
    // Therefore, we can be sure `self` is the only reference.
    if let Ok(wsr) = Rc::try_unwrap(self) {
      wsr.0.into_inner().finish();
    } else {
      panic!("Couldn't consume WasmStreamingResource.");
    }
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
  let view = BufMutView::from(buf);
  resource.read_byob(view).await.map(|(n, _)| n as u32)
}

#[op2(async)]
#[buffer]
async fn op_read_all(
  state: Rc<RefCell<OpState>>,
  #[smi] rid: ResourceId,
) -> Result<BytesMut, Error> {
  let resource = state.borrow().resource_table.get_any(rid)?;

  let (min, maybe_max) = resource.size_hint();
  let mut buffer_strategy =
    AdaptiveBufferStrategy::new_from_hint_u64(min, maybe_max);
  let mut buf = BufMutView::new(buffer_strategy.buffer_size());

  loop {
    #[allow(deprecated)]
    buf.maybe_grow(buffer_strategy.buffer_size()).unwrap();

    let (n, new_buf) = resource.clone().read_byob(buf).await?;
    buf = new_buf;
    buf.advance_cursor(n);
    if n == 0 {
      break;
    }

    buffer_strategy.notify_read(n);
  }

  let nread = buf.reset_cursor();
  // If the buffer is larger than the amount of data read, shrink it to the
  // amount of data read.
  buf.truncate(nread);

  Ok(buf.maybe_unwrap_bytes().unwrap())
}

#[op2(async)]
async fn op_write(
  state: Rc<RefCell<OpState>>,
  #[smi] rid: ResourceId,
  #[buffer] buf: JsBuffer,
) -> Result<u32, Error> {
  let resource = state.borrow().resource_table.get_any(rid)?;
  let view = BufView::from(buf);
  let resp = resource.write(view).await?;
  Ok(resp.nwritten() as u32)
}

#[op2(fast)]
fn op_read_sync(
  state: Rc<RefCell<OpState>>,
  #[smi] rid: ResourceId,
  #[buffer] data: &mut [u8],
) -> Result<u32, Error> {
  let resource = state.borrow_mut().resource_table.get_any(rid)?;
  resource.read_byob_sync(data).map(|n| n as u32)
}

#[op2(fast)]
fn op_write_sync(
  state: Rc<RefCell<OpState>>,
  #[smi] rid: ResourceId,
  #[buffer] data: &[u8],
) -> Result<u32, Error> {
  let resource = state.borrow_mut().resource_table.get_any(rid)?;
  let nwritten = resource.write_sync(data)?;
  Ok(nwritten as u32)
}

#[op2(async)]
async fn op_write_all(
  state: Rc<RefCell<OpState>>,
  #[smi] rid: ResourceId,
  #[buffer] buf: JsBuffer,
) -> Result<(), Error> {
  let resource = state.borrow().resource_table.get_any(rid)?;
  let view = BufView::from(buf);
  resource.write_all(view).await?;
  Ok(())
}

#[op2(async)]
async fn op_write_type_error(
  state: Rc<RefCell<OpState>>,
  #[smi] rid: ResourceId,
  #[string] error: String,
) -> Result<(), Error> {
  let resource = state.borrow().resource_table.get_any(rid)?;
  resource.write_error(type_error(error)).await?;
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

#[op2(fast)]
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

/// Creates a [`CancelHandle`] resource that can be used to cancel invocations of certain ops.
#[op2(fast)]
#[smi]
pub fn op_cancel_handle(state: &mut OpState) -> u32 {
  state.resource_table.add(CancelHandle::new())
}

#[op2]
#[serde]
fn op_encode_binary_string(#[buffer] s: &[u8]) -> ByteString {
  ByteString::from(s)
}

#[op2(fast)]
fn op_is_terminal(
  state: &mut OpState,
  #[smi] rid: ResourceId,
) -> Result<bool, Error> {
  let handle = state.resource_table.get_handle(rid)?;
  Ok(handle.is_terminal())
}
