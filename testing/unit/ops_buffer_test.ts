// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
import { assertArrayEquals, test } from "checkin:testing";
const {
  op_v8slice_store,
  op_v8slice_clone,
} = Deno.core.ensureFastOps();

// Cloning a buffer should result in the same buffer being returned
test(function testBufferStore() {
  const data = new Uint8Array(1024 * 1024);
  op_v8slice_store("buffer", data);
  const output = op_v8slice_clone("buffer");
  assertArrayEquals(output, new Uint8Array(1024 * 1024));
});

// Ensure that the returned buffer size is correct when a buffer is resized
// externally via `ArrayBuffer.transfer`.
test(function testBufferTransfer() {
  const data = new Uint8Array(1024 * 1024);
  const buffer = data.buffer;
  op_v8slice_store("buffer", data);
  buffer.transfer(100);
  const output = op_v8slice_clone("buffer");
  assertArrayEquals(output, new Uint8Array(100));
});
