// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
import { test } from "checkin:testing";
const {
  op_async_yield,
  op_async_barrier_create,
  op_async_barrier_await,
} = Deno.core.ensureFastOps();

test(async function testAsyncOp() {
  await op_async_yield();
});

// Test a large number of async ops resolving at the same time. This stress-tests both
// large-batch op dispatch and the JS-side promise-tracking implementation.
test(async function testAsyncBarrier() {
  const count = 1e6;
  op_async_barrier_create("barrier", count);
  const promises = [];
  for (let i = 0; i < count; i++) {
    promises.push(op_async_barrier_await("barrier"));
  }
  await Promise.all(promises);
});
