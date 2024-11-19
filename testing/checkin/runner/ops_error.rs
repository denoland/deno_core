// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
use anyhow::anyhow;
use anyhow::Error;
use deno_core::error::custom_error;
use deno_core::error::type_error;
use deno_core::op;

#[op(async)]
pub async fn op_async_throw_error_eager() -> Result<(), Error> {
  Err(type_error("Error"))
}

#[op(async(deferred), fast)]
pub async fn op_async_throw_error_deferred() -> Result<(), Error> {
  Err(type_error("Error"))
}

#[op(async(lazy), fast)]
pub async fn op_async_throw_error_lazy() -> Result<(), Error> {
  Err(type_error("Error"))
}

#[op(fast)]
pub fn op_error_custom_sync(#[string] message: String) -> Result<(), Error> {
  Err(custom_error("BadResource", message))
}

#[op(fast)]
pub fn op_error_context_sync(
  #[string] message: String,
  #[string] context: String,
) -> Result<(), Error> {
  Err(anyhow!(message).context(context))
}

#[op(async)]
pub async fn op_error_context_async(
  #[string] message: String,
  #[string] context: String,
) -> Result<(), Error> {
  Err(anyhow!(message).context(context))
}
