// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
use anyhow::anyhow;
use deno_core::error::JsNativeError;
use deno_core::error::OpError;
use deno_core::error::ResourceError;
use deno_core::op2;

#[op2(async)]
pub async fn op_async_throw_error_eager() -> Result<(), OpError> {
  Err(JsNativeError::type_error("Error").into())
}

#[op2(async(deferred), fast)]
pub async fn op_async_throw_error_deferred() -> Result<(), OpError> {
  Err(JsNativeError::type_error("Error").into())
}

#[op2(async(lazy), fast)]
pub async fn op_async_throw_error_lazy() -> Result<(), OpError> {
  Err(JsNativeError::type_error("Error").into())
}

#[op2(fast)]
pub fn op_error_custom_sync(#[string] message: String) -> Result<(), OpError> {
  Err(ResourceError::Other(message).into())
}

#[op2(fast)]
pub fn op_error_context_sync(
  #[string] message: String,
  #[string] context: String,
) -> Result<(), OpError> {
  Err(anyhow!(message).context(context).into())
}

#[op2(async)]
pub async fn op_error_context_async(
  #[string] message: String,
  #[string] context: String,
) -> Result<(), OpError> {
  Err(anyhow!(message).context(context).into())
}
