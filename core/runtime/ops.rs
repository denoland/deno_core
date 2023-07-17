// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use crate::ops::*;
use crate::OpResult;
use crate::PromiseId;
use anyhow::Error;
use futures::future::Either;
use futures::future::Future;
use futures::future::FutureExt;
use futures::task::noop_waker_ref;
use serde::Deserialize;
use serde::Serialize;
use serde_v8::from_v8;
use serde_v8::to_v8;
use std::borrow::Cow;
use std::cell::RefCell;
use std::future::ready;
use std::mem::MaybeUninit;
use std::option::Option;
use std::task::Context;
use std::task::Poll;

#[inline]
pub fn queue_fast_async_op<R: serde::Serialize + 'static>(
  ctx: &OpCtx,
  promise_id: PromiseId,
  op: impl Future<Output = Result<R, Error>> + 'static,
) {
  let get_class = {
    let state = RefCell::borrow(&ctx.state);
    state.tracker.track_async(ctx.id);
    state.get_error_class_fn
  };
  let fut = op.map(|result| crate::_ops::to_op_result(get_class, result));
  ctx
    .context_state
    .borrow_mut()
    .pending_ops
    .spawn(OpCall::new(ctx, promise_id, fut));
}

#[inline]
pub fn map_async_op1<R: serde::Serialize + 'static>(
  ctx: &OpCtx,
  op: impl Future<Output = Result<R, Error>> + 'static,
) -> impl Future<Output = OpResult> {
  let get_class = {
    let state = RefCell::borrow(&ctx.state);
    state.tracker.track_async(ctx.id);
    state.get_error_class_fn
  };

  op.map(|res| crate::_ops::to_op_result(get_class, res))
}

#[inline]
pub fn map_async_op2<R: serde::Serialize + 'static>(
  ctx: &OpCtx,
  op: impl Future<Output = R> + 'static,
) -> impl Future<Output = OpResult> {
  let state = RefCell::borrow(&ctx.state);
  state.tracker.track_async(ctx.id);

  op.map(|res| OpResult::Ok(res.into()))
}

#[inline]
pub fn map_async_op3<R: serde::Serialize + 'static>(
  ctx: &OpCtx,
  op: Result<impl Future<Output = Result<R, Error>> + 'static, Error>,
) -> impl Future<Output = OpResult> {
  let get_class = {
    let state = RefCell::borrow(&ctx.state);
    state.tracker.track_async(ctx.id);
    state.get_error_class_fn
  };

  match op {
    Err(err) => {
      Either::Left(ready(OpResult::Err(OpError::new(get_class, err))))
    }
    Ok(fut) => {
      Either::Right(fut.map(|res| crate::_ops::to_op_result(get_class, res)))
    }
  }
}

#[inline]
pub fn map_async_op4<R: serde::Serialize + 'static>(
  ctx: &OpCtx,
  op: Result<impl Future<Output = R> + 'static, Error>,
) -> impl Future<Output = OpResult> {
  let get_class = {
    let state = RefCell::borrow(&ctx.state);
    state.tracker.track_async(ctx.id);
    state.get_error_class_fn
  };

  match op {
    Err(err) => {
      Either::Left(ready(OpResult::Err(OpError::new(get_class, err))))
    }
    Ok(fut) => Either::Right(fut.map(|r| OpResult::Ok(r.into()))),
  }
}

pub fn queue_async_op<'s>(
  ctx: &OpCtx,
  scope: &'s mut v8::HandleScope,
  deferred: bool,
  promise_id: PromiseId,
  op: impl Future<Output = OpResult> + 'static,
) -> Option<v8::Local<'s, v8::Value>> {
  // An op's realm (as given by `OpCtx::realm_idx`) must match the realm in
  // which it is invoked. Otherwise, we might have cross-realm object exposure.
  // deno_core doesn't currently support such exposure, even though embedders
  // can cause them, so we panic in debug mode (since the check is expensive).
  // TODO(mmastrac): Restore this
  // debug_assert_eq!(
  //   runtime_state.borrow().context(ctx.realm_idx as usize, scope),
  //   Some(scope.get_current_context())
  // );

  let id = ctx.id;

  // TODO(mmastrac): We have to poll every future here because that assumption is baked into a large number
  // of ops. If we can figure out a way around this, we can remove this call to boxed_local and save a malloc per future.
  let mut pinned = op.map(move |res| (promise_id, id, res)).boxed_local();

  match pinned.poll_unpin(&mut Context::from_waker(noop_waker_ref())) {
    Poll::Pending => {}
    Poll::Ready(res) => {
      if deferred {
        ctx.context_state.borrow_mut().pending_ops.spawn(ready(res));
        return None;
      } else {
        ctx.state.borrow_mut().tracker.track_async_completed(ctx.id);
        return Some(res.2.to_v8(scope).unwrap());
      }
    }
  }

  ctx.context_state.borrow_mut().pending_ops.spawn(pinned);
  None
}

#[inline]
pub fn map_async_op_infallible<'a, R: 'static>(
  ctx: &OpCtx,
  promise_id: i32,
  op: impl Future<Output = R> + 'static,
  rv_map: for<'r> fn(
    &mut v8::HandleScope<'r>,
    R,
  ) -> Result<v8::Local<'r, v8::Value>, serde_v8::Error>,
) -> Option<R> {
  let id = ctx.id;

  // TODO(mmastrac): We have to poll every future here because that assumption is baked into a large number
  // of ops. If we can figure out a way around this, we can remove this call to boxed_local and save a malloc per future.
  let mut pinned = op.map(move |res| (promise_id, id, res)).boxed_local();

  match pinned.poll_unpin(&mut Context::from_waker(noop_waker_ref())) {
    Poll::Pending => {}
    Poll::Ready(res) => {
      ctx.state.borrow_mut().tracker.track_async_completed(ctx.id);
      return Some(res.2);
    }
  }

  ctx
    .context_state
    .borrow_mut()
    .pending_ops
    .spawn(pinned.map(move |r| {
      (
        r.0,
        r.1,
        OpResult::Op2Temp(Box::new(move |scope| rv_map(scope, r.2))),
      )
    }));
  None
}

// #[inline]
// pub fn map_async_op_fallible<'a, R, E>(
//   ctx: &OpCtx,
//   op: impl Future<Output = Result<R, E>>,
//   rv_map: fn(&'a mut v8::HandleScope, R) -> v8::Local<'a, v8::Value>,
// ) -> Option<Result<R, E>> {
//   None
// }

macro_rules! try_number {
  ($n:ident $type:ident $is:ident) => {
    if $n.$is() {
      // SAFETY: v8 handles can be transmuted
      let n: &v8::$type = unsafe { std::mem::transmute($n) };
      return n.value() as _;
    }
  };
}

pub fn to_u32(number: &v8::Value) -> u32 {
  try_number!(number Uint32 is_uint32);
  try_number!(number Int32 is_int32);
  try_number!(number Number is_number);
  if number.is_big_int() {
    // SAFETY: v8 handles can be transmuted
    let n: &v8::BigInt = unsafe { std::mem::transmute(number) };
    return n.u64_value().0 as _;
  }
  0
}

pub fn to_i32(number: &v8::Value) -> i32 {
  try_number!(number Uint32 is_uint32);
  try_number!(number Int32 is_int32);
  try_number!(number Number is_number);
  if number.is_big_int() {
    // SAFETY: v8 handles can be transmuted
    let n: &v8::BigInt = unsafe { std::mem::transmute(number) };
    return n.i64_value().0 as _;
  }
  0
}

pub fn to_u64(number: &v8::Value) -> u32 {
  try_number!(number Uint32 is_uint32);
  try_number!(number Int32 is_int32);
  try_number!(number Number is_number);
  if number.is_big_int() {
    // SAFETY: v8 handles can be transmuted
    let n: &v8::BigInt = unsafe { std::mem::transmute(number) };
    return n.u64_value().0 as _;
  }
  0
}

pub fn to_i64(number: &v8::Value) -> i32 {
  try_number!(number Uint32 is_uint32);
  try_number!(number Int32 is_int32);
  try_number!(number Number is_number);
  if number.is_big_int() {
    // SAFETY: v8 handles can be transmuted
    let n: &v8::BigInt = unsafe { std::mem::transmute(number) };
    return n.i64_value().0 as _;
  }
  0
}

pub fn to_f32(number: &v8::Value) -> f32 {
  try_number!(number Uint32 is_uint32);
  try_number!(number Int32 is_int32);
  try_number!(number Number is_number);
  if number.is_big_int() {
    // SAFETY: v8 handles can be transmuted
    let n: &v8::BigInt = unsafe { std::mem::transmute(number) };
    return n.i64_value().0 as _;
  }
  0.0
}

pub fn to_f64(number: &v8::Value) -> f64 {
  try_number!(number Uint32 is_uint32);
  try_number!(number Int32 is_int32);
  try_number!(number Number is_number);
  if number.is_big_int() {
    // SAFETY: v8 handles can be transmuted
    let n: &v8::BigInt = unsafe { std::mem::transmute(number) };
    return n.i64_value().0 as _;
  }
  0.0
}

/// Expands `inbuf` to `outbuf`, assuming that `outbuf` has at least 2x `input_length`.
#[inline(always)]
unsafe fn latin1_to_utf8(
  input_length: usize,
  inbuf: *const u8,
  outbuf: *mut u8,
) -> usize {
  let mut output = 0;
  let mut input = 0;
  while input < input_length {
    let char = *(inbuf.add(input));
    if char < 0x80 {
      *(outbuf.add(output)) = char;
      output += 1;
    } else {
      // Top two bits
      *(outbuf.add(output)) = (char >> 6) | 0b1100_0000;
      // Bottom six bits
      *(outbuf.add(output + 1)) = (char & 0b0011_1111) | 0b1000_0000;
      output += 2;
    }
    input += 1;
  }
  output
}

/// Converts a [`v8::fast_api::FastApiOneByteString`] to either an owned string, or a borrowed string, depending on whether it fits into the
/// provided buffer.
pub fn to_str_ptr<'a, const N: usize>(
  string: &mut v8::fast_api::FastApiOneByteString,
  buffer: &'a mut [MaybeUninit<u8>; N],
) -> Cow<'a, str> {
  let input_buf = string.as_bytes();
  let input_len = input_buf.len();
  let output_len = buffer.len();

  // We know that this string is full of either one or two-byte UTF-8 chars, so if it's < 1/2 of N we
  // can skip the ASCII check and just start copying.
  if input_len < N / 2 {
    debug_assert!(output_len >= input_len * 2);
    let buffer = buffer.as_mut_ptr() as *mut u8;

    let written =
      // SAFETY: We checked that buffer is at least 2x the size of input_buf
      unsafe { latin1_to_utf8(input_buf.len(), input_buf.as_ptr(), buffer) };

    debug_assert!(written <= output_len);

    let slice = std::ptr::slice_from_raw_parts(buffer, written);
    // SAFETY: We know it's valid UTF-8, so make a string
    Cow::Borrowed(unsafe { std::str::from_utf8_unchecked(&*slice) })
  } else {
    // TODO(mmastrac): We could be smarter here about not allocating
    Cow::Owned(to_string_ptr(string))
  }
}

/// Converts a [`v8::fast_api::FastApiOneByteString`] to an owned string. May over-allocate to avoid
/// re-allocation.
pub fn to_string_ptr(
  string: &mut v8::fast_api::FastApiOneByteString,
) -> String {
  let input_buf = string.as_bytes();
  let capacity = input_buf.len() * 2;

  // SAFETY: We're allocating a buffer of 2x the input size, writing valid UTF-8, then turning that into a string
  unsafe {
    // Create an uninitialized buffer of `capacity` bytes. We need to be careful here to avoid
    // accidentally creating a slice of u8 which would be invalid.
    let layout = std::alloc::Layout::from_size_align(capacity, 1).unwrap();
    let out = std::alloc::alloc(layout);

    let written = latin1_to_utf8(input_buf.len(), input_buf.as_ptr(), out);

    debug_assert!(written <= capacity);
    // We know it's valid UTF-8, so make a string
    String::from_raw_parts(out, written, capacity)
  }
}

/// Converts a [`v8::String`] to either an owned string, or a borrowed string, depending on whether it fits into the
/// provided buffer.
#[inline(always)]
pub fn to_str<'a, const N: usize>(
  scope: &mut v8::Isolate,
  string: &v8::Value,
  buffer: &'a mut [MaybeUninit<u8>; N],
) -> Cow<'a, str> {
  if !string.is_string() {
    return Cow::Borrowed("");
  }

  // SAFETY: We checked is_string above
  let string: &v8::String = unsafe { std::mem::transmute(string) };

  string.to_rust_cow_lossy(scope, buffer)
}

/// Converts from a raw [`v8::Value`] to the expected V8 data type.
#[inline(always)]
#[allow(clippy::result_unit_err)]
pub fn v8_try_convert<'a, T>(
  value: v8::Local<'a, v8::Value>,
) -> Result<v8::Local<'a, T>, ()>
where
  v8::Local<'a, T>: TryFrom<v8::Local<'a, v8::Value>>,
{
  v8::Local::<T>::try_from(value).map_err(drop)
}

/// Converts from a raw [`v8::Value`] to the expected V8 data type, wrapped in an [`Option`].
#[inline(always)]
#[allow(clippy::result_unit_err)]
pub fn v8_try_convert_option<'a, T>(
  value: v8::Local<'a, v8::Value>,
) -> Result<Option<v8::Local<'a, T>>, ()>
where
  v8::Local<'a, T>: TryFrom<v8::Local<'a, v8::Value>>,
{
  if value.is_null_or_undefined() {
    Ok(None)
  } else {
    Ok(Some(v8::Local::<T>::try_from(value).map_err(drop)?))
  }
}

pub fn serde_rust_to_v8<'a, T: Serialize>(
  scope: &mut v8::HandleScope<'a>,
  input: T,
) -> serde_v8::Result<v8::Local<'a, v8::Value>> {
  to_v8(scope, input)
}

pub fn serde_v8_to_rust<'a, T: Deserialize<'a>>(
  scope: &mut v8::HandleScope,
  input: v8::Local<v8::Value>,
) -> serde_v8::Result<T> {
  from_v8(scope, input)
}

/// Retrieve a [`serde_v8::V8Slice`] from a typed array in an [`v8::ArrayBufferView`].
pub fn to_v8_slice<'a, T>(
  scope: &mut v8::HandleScope,
  input: v8::Local<'a, v8::Value>,
) -> Result<serde_v8::V8Slice, &'static str>
where
  v8::Local<'a, T>: TryFrom<v8::Local<'a, v8::Value>>,
  v8::Local<'a, v8::ArrayBufferView>: From<v8::Local<'a, T>>,
{
  let (store, offset, length) = if let Ok(buf) = v8::Local::<T>::try_from(input)
  {
    let buf: v8::Local<v8::ArrayBufferView> = buf.into();
    let Some(buffer) = buf.buffer(scope) else {
      return Err("buffer missing");
    };
    (
      buffer.get_backing_store(),
      buf.byte_offset(),
      buf.byte_length(),
    )
  } else {
    return Err("expected typed ArrayBufferView");
  };
  let slice =
    unsafe { serde_v8::V8Slice::from_parts(store, offset..(offset + length)) };
  Ok(slice)
}

#[cfg(test)]
mod tests {
  use crate::error::generic_error;
  use crate::error::AnyError;
  use crate::error::JsError;
  use crate::FastString;
  use crate::JsRuntime;
  use crate::OpState;
  use crate::RuntimeOptions;
  use deno_ops::op2;
  use futures::Future;
  use serde::Deserialize;
  use serde::Serialize;
  use std::borrow::Cow;
  use std::cell::Cell;
  use std::cell::RefCell;
  use std::rc::Rc;
  use std::time::Duration;

  crate::extension!(
    testing,
    ops = [
      op_test_fail,
      op_test_print_debug,

      op_test_add,
      op_test_add_option,
      op_test_result_void_switch,
      op_test_result_void_ok,
      op_test_result_void_err,
      op_test_result_primitive_ok,
      op_test_result_primitive_err,
      op_test_bool,
      op_test_bool_result,
      op_test_float,
      op_test_float_result,
      op_test_string_owned,
      op_test_string_ref,
      op_test_string_cow,
      op_test_string_roundtrip_char,
      op_test_string_return,
      op_test_string_option_return,
      op_test_string_roundtrip,
      op_test_generics<String>,
      op_test_v8_types,
      op_test_v8_option_string,
      op_test_v8_type_return,
      op_test_v8_type_return_option,
      op_test_v8_type_handle_scope,
      op_test_v8_type_handle_scope_obj,
      op_test_v8_type_handle_scope_result,
      op_test_serde_v8,
      op_state_rc,
      op_state_ref,
      op_state_mut,
      op_state_mut_attr,
      op_state_multi_attr,
      op_buffer_slice,
      op_buffer_slice_unsafe_callback,
      op_buffer_copy,

      op_async_void,
      op_async_number,
      op_async_add,
      op_async_sleep,
      op_async_sleep_impl,
      op_async_buffer_impl,
    ],
    state = |state| {
      state.put(1234u32);
      state.put(10000u16);
    }
  );

  thread_local! {
    static FAIL: Cell<bool> = Cell::new(false)
  }

  #[op2(core, fast)]
  pub fn op_test_fail() {
    FAIL.with(|b| b.set(true))
  }

  #[op2(core, fast)]
  pub fn op_test_print_debug(#[string] s: &str) {
    println!("{s}")
  }

  /// Run a test for a single op.
  fn run_test2(repeat: usize, op: &str, test: &str) -> Result<(), AnyError> {
    let mut runtime = JsRuntime::new(RuntimeOptions {
      extensions: vec![testing::init_ops_and_esm()],
      ..Default::default()
    });
    let err_mapper =
      |err| generic_error(format!("{op} test failed ({test}): {err:?}"));
    runtime
      .execute_script(
        "",
        FastString::Owned(
          format!(
            r"
            const {{ op_test_fail, op_test_print_debug, {op} }} = Deno.core.ensureFastOps();
            function assert(b) {{
              if (!b) {{
                op_test_fail();
              }}
            }}
            function log(s) {{
              op_test_print_debug(String(s))
            }}
          "
          )
          .into(),
        ),
      )
      .map_err(err_mapper)?;
    FAIL.with(|b| b.set(false));
    runtime.execute_script(
      "",
      FastString::Owned(
        format!(
          r"
      for (let __index__ = 0; __index__ < {repeat}; __index__++) {{
        {test}
      }}
    "
        )
        .into(),
      ),
    )?;
    if FAIL.with(|b| b.get()) {
      Err(generic_error(format!("{op} test failed ({test})")))
    } else {
      Ok(())
    }
  }

  /// Run a test for a single op.
  async fn run_async_test(
    repeat: usize,
    op: &str,
    test: &str,
  ) -> Result<(), AnyError> {
    let mut runtime = JsRuntime::new(RuntimeOptions {
      extensions: vec![testing::init_ops_and_esm()],
      ..Default::default()
    });
    let err_mapper =
      |err| generic_error(format!("{op} test failed ({test}): {err:?}"));
    runtime
      .execute_script(
        "",
        FastString::Owned(
          format!(
            r"
            const {{ op_test_fail, op_test_print_debug, {op} }} = Deno.core.ensureFastOps();
            function assert(b) {{
              if (!b) {{
                op_test_fail();
              }}
            }}
            function log(s) {{
              op_test_print_debug(String(s))
            }}
          "
          )
          .into(),
        ),
      )
      .map_err(err_mapper)?;
    FAIL.with(|b| b.set(false));
    runtime.execute_script(
      "",
      FastString::Owned(
        format!(
          r"
            (async () => {{
              for (let __index__ = 0; __index__ < {repeat}; __index__++) {{
                {test}
              }}
            }})()
          "
        )
        .into(),
      ),
    )?;

    runtime.run_event_loop(false).await?;
    if FAIL.with(|b| b.get()) {
      Err(generic_error(format!("{op} test failed ({test})")))
    } else {
      Ok(())
    }
  }

  #[tokio::test(flavor = "current_thread")]
  pub async fn test_op_fail() {
    assert!(run_test2(1, "", "assert(false)").is_err());
  }

  #[op2(core, fast)]
  pub fn op_test_add(a: u32, b: i32) -> u32 {
    (a as i32 + b) as u32
  }

  /// Test various numeric coercions in fast and slow mode.
  #[tokio::test(flavor = "current_thread")]
  pub async fn test_op_add() -> Result<(), Box<dyn std::error::Error>> {
    run_test2(10000, "op_test_add", "assert(op_test_add(1, 11) == 12)")?;
    run_test2(10000, "op_test_add", "assert(op_test_add(11, -1) == 10)")?;
    run_test2(10000, "op_test_add", "assert(op_test_add(1.5, 11.5) == 12)")?;
    run_test2(10000, "op_test_add", "assert(op_test_add(11.5, -1) == 10)")?;
    run_test2(
      10000,
      "op_test_add",
      "assert(op_test_add(4096n, 4096n) == 4096 + 4096)",
    )?;
    run_test2(
      10000,
      "op_test_add",
      "assert(op_test_add(8192n, -4096n) == 4096)",
    )?;
    Ok(())
  }

  #[op2(core)]
  pub fn op_test_add_option(a: u32, b: Option<u32>) -> u32 {
    a + b.unwrap_or(100)
  }

  #[tokio::test(flavor = "current_thread")]
  pub async fn test_op_add_option() -> Result<(), Box<dyn std::error::Error>> {
    // This isn't fast, so we don't repeat it
    run_test2(
      1,
      "op_test_add_option",
      "assert(op_test_add_option(1, 11) == 12)",
    )?;
    run_test2(
      1,
      "op_test_add_option",
      "assert(op_test_add_option(1, null) == 101)",
    )?;
    Ok(())
  }

  thread_local! {
    static RETURN_COUNT: Cell<usize> = Cell::new(0);
  }

  #[op2(core, fast)]
  pub fn op_test_result_void_switch() -> Result<(), AnyError> {
    let count = RETURN_COUNT.with(|count| {
      let new = count.get() + 1;
      count.set(new);
      new
    });
    if count > 5000 {
      Err(generic_error("failed!!!"))
    } else {
      Ok(())
    }
  }

  #[op2(core, fast)]
  pub fn op_test_result_void_err() -> Result<(), AnyError> {
    Err(generic_error("failed!!!"))
  }

  #[op2(core, fast)]
  pub fn op_test_result_void_ok() -> Result<(), AnyError> {
    Ok(())
  }

  #[tokio::test(flavor = "current_thread")]
  pub async fn test_op_result_void() -> Result<(), Box<dyn std::error::Error>> {
    // Test the non-switching kinds
    run_test2(
      10000,
      "op_test_result_void_err",
      "try { op_test_result_void_err(); assert(false) } catch (e) {}",
    )?;
    run_test2(10000, "op_test_result_void_ok", "op_test_result_void_ok()")?;
    Ok(())
  }

  #[tokio::test(flavor = "current_thread")]
  pub async fn test_op_result_void_switch(
  ) -> Result<(), Box<dyn std::error::Error>> {
    RETURN_COUNT.with(|count| count.set(0));
    let err = run_test2(
      10000,
      "op_test_result_void_switch",
      "op_test_result_void_switch();",
    )
    .expect_err("Expected this to fail");
    let js_err = err.downcast::<JsError>().unwrap();
    assert_eq!(js_err.message, Some("failed!!!".into()));
    assert_eq!(RETURN_COUNT.with(|count| count.get()), 5001);
    Ok(())
  }

  #[op2(core, fast)]
  pub fn op_test_result_primitive_err() -> Result<u32, AnyError> {
    Err(generic_error("failed!!!"))
  }

  #[op2(core, fast)]
  pub fn op_test_result_primitive_ok() -> Result<u32, AnyError> {
    Ok(123)
  }

  #[tokio::test]
  pub async fn test_op_result_primitive(
  ) -> Result<(), Box<dyn std::error::Error>> {
    run_test2(
      10000,
      "op_test_result_primitive_err",
      "try { op_test_result_primitive_err(); assert(false) } catch (e) {}",
    )?;
    run_test2(
      10000,
      "op_test_result_primitive_ok",
      "op_test_result_primitive_ok()",
    )?;
    Ok(())
  }

  #[op2(core, fast)]
  pub fn op_test_bool(b: bool) -> bool {
    b
  }

  #[op2(core, fast)]
  pub fn op_test_bool_result(b: bool) -> Result<bool, AnyError> {
    if b {
      Ok(true)
    } else {
      Err(generic_error("false!!!"))
    }
  }

  #[tokio::test]
  pub async fn test_op_bool() -> Result<(), Box<dyn std::error::Error>> {
    run_test2(
      10000,
      "op_test_bool",
      "assert(op_test_bool(true) === true && op_test_bool(false) === false)",
    )?;
    run_test2(
      10000,
      "op_test_bool_result",
      "assert(op_test_bool_result(true) === true)",
    )?;
    run_test2(
      1,
      "op_test_bool_result",
      "try { op_test_bool_result(false); assert(false) } catch (e) {}",
    )?;
    Ok(())
  }

  #[op2(core, fast)]
  pub fn op_test_float(a: f32, b: f64) -> f32 {
    a + b as f32
  }

  #[op2(core, fast)]
  pub fn op_test_float_result(a: f32, b: f64) -> Result<f64, AnyError> {
    let a = a as f64;
    if a + b >= 0. {
      Ok(a + b)
    } else {
      Err(generic_error("negative!!!"))
    }
  }

  #[tokio::test]
  pub async fn test_op_float() -> Result<(), Box<dyn std::error::Error>> {
    run_test2(10000, "op_test_float", "assert(op_test_float(1, 10) == 11)")?;
    run_test2(
      10000,
      "op_test_float_result",
      "assert(op_test_float_result(1, 10) == 11)",
    )?;
    run_test2(
      1,
      "op_test_float_result",
      "try { op_test_float_result(-1, -1); assert(false) } catch (e) {}",
    )?;
    Ok(())
  }

  #[op2(core, fast)]
  pub fn op_test_string_owned(#[string] s: String) -> u32 {
    s.len() as _
  }

  #[op2(core, fast)]
  pub fn op_test_string_ref(#[string] s: &str) -> u32 {
    s.len() as _
  }

  #[op2(core, fast)]
  pub fn op_test_string_cow(#[string] s: Cow<str>) -> u32 {
    s.len() as _
  }

  #[op2(core, fast)]
  pub fn op_test_string_roundtrip_char(#[string] s: Cow<str>) -> u32 {
    s.chars().next().unwrap() as u32
  }

  #[tokio::test]
  pub async fn test_op_strings() -> Result<(), Box<dyn std::error::Error>> {
    for op in [
      "op_test_string_owned",
      "op_test_string_cow",
      "op_test_string_ref",
    ] {
      for (len, str) in [
        // ASCII
        (3, "'abc'"),
        // Latin-1 (one byte but two UTF-8 chars)
        (2, "'\\u00a0'"),
        // ASCII
        (10000, "'a'.repeat(10000)"),
        // Latin-1
        (20000, "'\\u00a0'.repeat(10000)"),
        // 4-byte UTF-8 emoji (1F995 = 🦕)
        (40000, "'\\u{1F995}'.repeat(10000)"),
      ] {
        let test = format!("assert({op}({str}) == {len})");
        run_test2(10000, op, &test)?;
      }
    }

    // Ensure that we're correctly encoding UTF-8
    run_test2(
      10000,
      "op_test_string_roundtrip_char",
      "assert(op_test_string_roundtrip_char('\\u00a0') == 0xa0)",
    )?;
    run_test2(
      10000,
      "op_test_string_roundtrip_char",
      "assert(op_test_string_roundtrip_char('\\u00ff') == 0xff)",
    )?;
    run_test2(
      10000,
      "op_test_string_roundtrip_char",
      "assert(op_test_string_roundtrip_char('\\u0080') == 0x80)",
    )?;
    run_test2(
      10000,
      "op_test_string_roundtrip_char",
      "assert(op_test_string_roundtrip_char('\\u0100') == 0x100)",
    )?;
    Ok(())
  }

  #[op2(core)]
  #[string]
  pub fn op_test_string_return(
    #[string] a: Cow<str>,
    #[string] b: Cow<str>,
  ) -> String {
    (a + b).to_string()
  }

  #[op2(core)]
  #[string]
  pub fn op_test_string_option_return(
    #[string] a: Cow<str>,
    #[string] b: Cow<str>,
  ) -> Option<String> {
    if a == "none" {
      return None;
    }
    Some((a + b).to_string())
  }

  #[op2(core)]
  #[string]
  pub fn op_test_string_roundtrip(#[string] s: String) -> String {
    s
  }

  #[tokio::test]
  pub async fn test_op_string_returns() -> Result<(), Box<dyn std::error::Error>>
  {
    run_test2(
      1,
      "op_test_string_return",
      "assert(op_test_string_return('a', 'b') == 'ab')",
    )?;
    run_test2(
      1,
      "op_test_string_option_return",
      "assert(op_test_string_option_return('a', 'b') == 'ab')",
    )?;
    run_test2(
      1,
      "op_test_string_option_return",
      "assert(op_test_string_option_return('none', 'b') == null)",
    )?;
    run_test2(
      1,
      "op_test_string_roundtrip",
      "assert(op_test_string_roundtrip('\\u0080\\u00a0\\u00ff') == '\\u0080\\u00a0\\u00ff')",
    )?;
    Ok(())
  }

  // We don't actually test this one -- we just want it to compile
  #[op2(core, fast)]
  pub fn op_test_generics<T: Clone>() {}

  /// Tests v8 types without a handle scope
  #[allow(clippy::needless_lifetimes)]
  #[op2(core, fast)]
  pub fn op_test_v8_types<'s>(
    s: &v8::String,
    s2: v8::Local<v8::String>,
    s3: v8::Local<'s, v8::String>,
  ) -> u32 {
    if s.same_value(s2.into()) {
      1
    } else if s.same_value(s3.into()) {
      2
    } else {
      3
    }
  }

  #[op2(core, fast)]
  pub fn op_test_v8_option_string(s: Option<&v8::String>) -> i32 {
    if let Some(s) = s {
      s.length() as i32
    } else {
      -1
    }
  }

  /// Tests v8 types without a handle scope
  #[op2(core)]
  #[allow(clippy::needless_lifetimes)]
  pub fn op_test_v8_type_return<'s>(
    s: v8::Local<'s, v8::String>,
  ) -> v8::Local<'s, v8::String> {
    s
  }

  /// Tests v8 types without a handle scope
  #[op2(core)]
  #[allow(clippy::needless_lifetimes)]
  pub fn op_test_v8_type_return_option<'s>(
    s: Option<v8::Local<'s, v8::String>>,
  ) -> Option<v8::Local<'s, v8::String>> {
    s
  }

  #[op2(core)]
  pub fn op_test_v8_type_handle_scope<'s>(
    scope: &mut v8::HandleScope<'s>,
    s: &v8::String,
  ) -> v8::Local<'s, v8::String> {
    let s = s.to_rust_string_lossy(scope);
    v8::String::new(scope, &s).unwrap()
  }

  /// Extract whatever lives in "key" from the object.
  #[op2(core)]
  pub fn op_test_v8_type_handle_scope_obj<'s>(
    scope: &mut v8::HandleScope<'s>,
    o: &v8::Object,
  ) -> Option<v8::Local<'s, v8::Value>> {
    let key = v8::String::new(scope, "key").unwrap().into();
    o.get(scope, key)
  }

  /// Extract whatever lives in "key" from the object.
  #[op2(core)]
  pub fn op_test_v8_type_handle_scope_result<'s>(
    scope: &mut v8::HandleScope<'s>,
    o: &v8::Object,
  ) -> Result<v8::Local<'s, v8::Value>, AnyError> {
    let key = v8::String::new(scope, "key").unwrap().into();
    o.get(scope, key)
      .filter(|v| !v.is_null_or_undefined())
      .ok_or(generic_error("error!!!"))
  }

  #[tokio::test]
  pub async fn test_op_v8_types() -> Result<(), Box<dyn std::error::Error>> {
    for (a, b) in [("a", 1), ("b", 2), ("c", 3)] {
      run_test2(
        10000,
        "op_test_v8_types",
        &format!("assert(op_test_v8_types('{a}', 'a', 'b') == {b})"),
      )?;
    }
    // Fast ops
    for (a, b, c) in [
      ("op_test_v8_option_string", "'xyz'", "3"),
      ("op_test_v8_option_string", "null", "-1"),
    ] {
      run_test2(10000, a, &format!("assert({a}({b}) == {c})"))?;
    }
    // Non-fast ops
    for (a, b, c) in [
      ("op_test_v8_type_return", "'xyz'", "'xyz'"),
      ("op_test_v8_type_return_option", "'xyz'", "'xyz'"),
      ("op_test_v8_type_return_option", "null", "null"),
      ("op_test_v8_type_handle_scope", "'xyz'", "'xyz'"),
      ("op_test_v8_type_handle_scope_obj", "{'key': 1}", "1"),
      (
        "op_test_v8_type_handle_scope_obj",
        "{'key': 'abc'}",
        "'abc'",
      ),
      (
        "op_test_v8_type_handle_scope_obj",
        "{'no_key': 'abc'}",
        "null",
      ),
      (
        "op_test_v8_type_handle_scope_result",
        "{'key': 'abc'}",
        "'abc'",
      ),
    ] {
      run_test2(1, a, &format!("assert({a}({b}) == {c})"))?;
    }

    // Test the error case for op_test_v8_type_handle_scope_result
    run_test2(1, "op_test_v8_type_handle_scope_result", "try { op_test_v8_type_handle_scope_result({}); assert(false); } catch (e) {}")?;
    Ok(())
  }

  #[derive(Serialize, Deserialize)]
  pub struct Serde {
    pub s: String,
  }

  #[op2(core)]
  #[serde]
  pub fn op_test_serde_v8(#[serde] mut serde: Serde) -> Serde {
    serde.s += "!";
    serde
  }

  #[tokio::test]
  pub async fn test_op_serde_v8() -> Result<(), Box<dyn std::error::Error>> {
    run_test2(
      1,
      "op_test_serde_v8",
      "assert(op_test_serde_v8({s: 'abc'}).s == 'abc!')",
    )?;
    run_test2(
      1,
      "op_test_serde_v8",
      "try { op_test_serde_v8({}); assert(false) } catch (e) { assert(String(e).indexOf('missing field') != -1) }",
    )?;
    Ok(())
  }

  #[op2(core, fast)]
  pub fn op_state_rc(state: Rc<RefCell<OpState>>, value: u32) -> u32 {
    let old_value: u32 = state.borrow_mut().take();
    state.borrow_mut().put(value);
    old_value
  }

  #[op2(core, fast)]
  pub fn op_state_ref(state: &OpState) -> u32 {
    let old_value: &u32 = state.borrow();
    *old_value
  }

  #[op2(core, fast)]
  pub fn op_state_mut(state: &mut OpState, value: u32) {
    *state.borrow_mut() = value;
  }

  #[op2(core, fast)]
  pub fn op_state_mut_attr(#[state] value: &mut u32, new_value: u32) -> u32 {
    let old_value = *value;
    *value = new_value;
    old_value
  }

  #[op2(core, fast)]
  pub fn op_state_multi_attr(
    #[state] value32: &u32,
    #[state] value16: &u16,
    #[state] value8: Option<&u8>,
  ) -> u32 {
    assert_eq!(value8, None);
    *value32 + *value16 as u32
  }

  #[tokio::test]
  pub async fn test_op_state() -> Result<(), Box<dyn std::error::Error>> {
    run_test2(
      10000,
      "op_state_rc",
      "if (__index__ == 0) { op_state_rc(__index__) } else { assert(op_state_rc(__index__) == __index__ - 1) }",
    )?;
    run_test2(
      10000,
      "op_state_mut_attr",
      "if (__index__ == 0) { op_state_mut_attr(__index__) } else { assert(op_state_mut_attr(__index__) == __index__ - 1) }",
    )?;
    run_test2(10000, "op_state_mut", "op_state_mut(__index__)")?;
    run_test2(10000, "op_state_ref", "assert(op_state_ref() == 1234)")?;
    run_test2(
      10000,
      "op_state_multi_attr",
      "assert(op_state_multi_attr() == 11234)",
    )?;
    Ok(())
  }

  #[op2(core, fast)]
  pub fn op_buffer_slice(
    #[buffer] input: &[u8],
    inlen: usize,
    #[buffer] output: &mut [u8],
    outlen: usize,
  ) {
    assert_eq!(inlen, input.len());
    assert_eq!(outlen, output.len());
    output[0] = input[0];
  }

  #[tokio::test]
  pub async fn test_op_buffer_slice() -> Result<(), Box<dyn std::error::Error>>
  {
    // Uint8Array -> Uint8Array
    run_test2(
      10000,
      "op_buffer_slice",
      r"
      let out = new Uint8Array(10);
      op_buffer_slice(new Uint8Array([1,2,3]), 3, out, 10);
      assert(out[0] == 1);",
    )?;
    // Uint8Array(ArrayBuffer) -> Uint8Array(ArrayBuffer)
    run_test2(
      10000,
      "op_buffer_slice",
      r"
      let inbuf = new ArrayBuffer(10);
      let in_u8 = new Uint8Array(inbuf);
      in_u8[0] = 1;
      let out = new ArrayBuffer(10);
      op_buffer_slice(in_u8, 10, new Uint8Array(out), 10);
      assert(new Uint8Array(out)[0] == 1);",
    )?;
    // Uint8Array(ArrayBuffer, 5, 5) -> Uint8Array(ArrayBuffer)
    run_test2(
      10000,
      "op_buffer_slice",
      r"
      let inbuf = new ArrayBuffer(10);
      let in_u8 = new Uint8Array(inbuf);
      in_u8[5] = 1;
      let out = new ArrayBuffer(10);
      op_buffer_slice(new Uint8Array(inbuf, 5, 5), 5, new Uint8Array(out), 10);
      assert(new Uint8Array(out)[0] == 1);",
    )?;
    // Resizable
    run_test2(
      10000,
      "op_buffer_slice",
      r"
      let inbuf = new ArrayBuffer(10, { maxByteLength: 100 });
      let in_u8 = new Uint8Array(inbuf);
      in_u8[5] = 1;
      let out = new ArrayBuffer(10, { maxByteLength: 100 });
      op_buffer_slice(new Uint8Array(inbuf, 5, 5), 5, new Uint8Array(out), 10);
      assert(new Uint8Array(out)[0] == 1);",
    )?;
    Ok(())
  }

  // TODO(mmastrac): This is a dangerous op that we'll use to test resizable buffers in a later pass.
  #[op2(core)]
  pub fn op_buffer_slice_unsafe_callback(
    scope: &mut v8::HandleScope,
    buffer: v8::Local<v8::ArrayBuffer>,
    callback: v8::Local<v8::Function>,
  ) {
    println!("{:?}", buffer.data());
    let recv = callback.into();
    callback.call(scope, recv, &[]);
    println!("{:?}", buffer.data());
  }

  #[ignore]
  #[tokio::test]
  async fn test_op_unsafe() -> Result<(), Box<dyn std::error::Error>> {
    run_test2(
      1,
      "op_buffer_slice_unsafe_callback",
      r"
      let inbuf = new ArrayBuffer(1024 * 1024, { maxByteLength: 10 * 1024 * 1024 });
      op_buffer_slice_unsafe_callback(inbuf, () => {
        inbuf.resize(0);
      });
      ",
    )?;
    Ok(())
  }

  /// Ensures that three copies are independent. Note that we cannot mutate the
  /// `bytes::Bytes`.
  #[op2(core, fast)]
  #[allow(clippy::boxed_local)] // Clippy bug? It warns about input2
  pub fn op_buffer_copy(
    #[buffer(copy)] mut input1: Vec<u8>,
    #[buffer(copy)] mut input2: Box<[u8]>,
    #[buffer(copy)] input3: bytes::Bytes,
  ) {
    assert_eq!(input1[0], input2[0]);
    assert_eq!(input2[0], input3[0]);
    input1[0] = 0xff;
    assert_ne!(input1[0], input2[0]);
    assert_eq!(input2[0], input3[0]);
    input2[0] = 0xff;
    assert_eq!(input1[0], input2[0]);
    assert_ne!(input2[0], input3[0]);
  }

  #[tokio::test]
  pub async fn test_op_buffer_copy() -> Result<(), Box<dyn std::error::Error>> {
    run_test2(
      10000,
      "op_buffer_copy",
      r"
      let input = new Uint8Array(10);
      input[0] = 1;
      op_buffer_copy(input, input, input);
      assert(input[0] == 1);",
    )?;
    Ok(())
  }

  #[op2(core)]
  async fn op_async_void() {}

  #[tokio::test]
  pub async fn test_op_async_void() -> Result<(), Box<dyn std::error::Error>> {
    run_async_test(10000, "op_async_void", "await op_async_void()").await?;
    Ok(())
  }

  #[op2(core)]
  async fn op_async_number(x: u32) -> u32 {
    x
  }

  #[op2(core)]
  async fn op_async_add(x: u32, y: u32) -> u32 {
    x + y
  }

  #[tokio::test]
  pub async fn test_op_async_number() -> Result<(), Box<dyn std::error::Error>>
  {
    run_async_test(
      10000,
      "op_async_number",
      "assert(await op_async_number(__index__) == __index__)",
    )
    .await?;
    run_async_test(
      10000,
      "op_async_add",
      "assert(await op_async_add(__index__, 100) == __index__ + 100)",
    )
    .await?;
    Ok(())
  }

  #[op2(core)]
  async fn op_async_sleep() {
    tokio::time::sleep(Duration::from_millis(500)).await
  }

  #[op2(core)]
  fn op_async_sleep_impl() -> impl Future<Output = ()> {
    tokio::time::sleep(Duration::from_millis(500))
  }

  #[tokio::test]
  pub async fn test_op_async_sleep() -> Result<(), Box<dyn std::error::Error>> {
    run_async_test(5, "op_async_sleep", "await op_async_sleep()").await?;
    run_async_test(5, "op_async_sleep_impl", "await op_async_sleep_impl()")
      .await?;
    Ok(())
  }

  #[op2(core)]
  fn op_async_buffer_impl(#[buffer] input: &[u8]) -> impl Future<Output = u32> {
    let l = input.len();
    async move { l as _ }
  }

  #[tokio::test]
  pub async fn test_op_async_buffer() -> Result<(), Box<dyn std::error::Error>>
  {
    run_async_test(
      5,
      "op_async_buffer_impl",
      "assert(await op_async_buffer_impl(new Uint8Array(10)) == 10)",
    )
    .await?;
    Ok(())
  }
}
