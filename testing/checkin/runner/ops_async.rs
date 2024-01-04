// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use deno_core::op2;

#[op2(async)]
pub async fn op_async_void_deferred(
) {
  tokio::task::yield_now().await
}

