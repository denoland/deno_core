// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
use deno_core::error::JsNativeError;
use deno_core::op2;

#[op2(async)]
pub async fn op_async_throw_error_eager() -> Result<(), JsNativeError> {
  Err(JsNativeError::type_error("Error"))
}

#[op2(async(deferred), fast)]
pub async fn op_async_throw_error_deferred() -> Result<(), JsNativeError> {
  Err(JsNativeError::type_error("Error"))
}

#[op2(async(lazy), fast)]
pub async fn op_async_throw_error_lazy() -> Result<(), JsNativeError> {
  Err(JsNativeError::type_error("Error"))
}

#[op2(fast)]
pub fn op_error_custom_sync(
  #[string] message: String,
) -> Result<(), JsNativeError> {
  Err(JsNativeError::generic(message))
}
