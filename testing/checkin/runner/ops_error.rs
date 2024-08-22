// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
use anyhow::anyhow;
use anyhow::Error;
use deno_core::error::JsNativeError;
use deno_core::op2;

#[op2(async)]
pub async fn op_async_throw_error_eager() -> Result<(), Error> {
  Err(JsNativeError::type_error("Error").into())
}

#[op2(async(deferred), fast)]
pub async fn op_async_throw_error_deferred() -> Result<(), Error> {
  Err(JsNativeError::type_error("Error").into())
}

#[op2(async(lazy), fast)]
pub async fn op_async_throw_error_lazy() -> Result<(), Error> {
  Err(JsNativeError::type_error("Error").into())
}

#[op2(fast)]
pub fn op_error_custom_sync(#[string] message: String) -> Result<(), Error> {
  Err(JsNativeError::bad_resource(message).into())
}

#[op2(fast)]
pub fn op_error_context_sync(
  #[string] message: String,
  #[string] context: String,
) -> Result<(), Error> {
  Err(anyhow!(message).context(context))
}

#[op2(async)]
pub async fn op_error_context_async(
  #[string] message: String,
  #[string] context: String,
) -> Result<(), Error> {
  Err(anyhow!(message).context(context))
}
