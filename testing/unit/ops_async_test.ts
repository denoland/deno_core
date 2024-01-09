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

test(async function testAsyncBarrier() {
  const count = 1e6;
  let barrier = op_async_barrier_create("barrier", count);
  let promises = [];
  for (let i = 0; i < count; i++) {
    promises.push(op_async_barrier_await("barrier"));
  }
  await Promise.all(promises);
});
