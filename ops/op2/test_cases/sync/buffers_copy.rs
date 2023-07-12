// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.

#[op2(fast)]
fn op_buffers(#[buffer(copy)] a: Vec<u8>, #[buffer(copy)] b: Box<[u8]>, #[buffer(copy)] c: bytes::Bytes) {
}
