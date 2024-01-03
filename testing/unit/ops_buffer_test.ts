// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
import { assertArrayEquals, assertEquals, test } from "checkin:testing";
const {
  op_v8slice_store,
  op_v8slice_clone
} = Deno.core.ensureFastOps();

test(function testBufferStore() {
  const data = new Uint8Array(1024*1024);
  const buffer = data.buffer;
  op_v8slice_store("buffer", data);
  const output = op_v8slice_clone("buffer");
  assertArrayEquals(output, new Uint8Array(1024*1024));
});

test(function testBufferTransfer() {
  const data = new Uint8Array(1024*1024);
  const buffer = data.buffer;
  op_v8slice_store("buffer", data);
  buffer.transfer(100);
  const output = op_v8slice_clone("buffer");
  assertArrayEquals(output, new Uint8Array(100));
});
