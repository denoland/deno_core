// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();

#[op2(async)]
#[meta(sanitizer_details = "read from a Blob or File")]
#[meta(sanitizer_fix = "awaiting the result of a Blob or File read")]
async fn op_blob_read_part() {
  return;
}

#[op2(async)]
#[meta(
  sanitizer_details = "receive a message from a BroadcastChannel",
  sanitizer_fix = "closing the BroadcastChannel"
)]
async fn op_broadcast_recv() {
  return;
}
