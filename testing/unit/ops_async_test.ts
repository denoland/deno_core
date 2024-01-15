// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
import { test } from "checkin:testing";
import { asyncYield, barrierAwait, barrierCreate } from "checkin:async";

test(async function testAsyncOp() {
  await asyncYield();
});

// Test a large number of async ops resolving at the same time. This stress-tests both
// large-batch op dispatch and the JS-side promise-tracking implementation.
test(async function testAsyncBarrier() {
  const count = 1e6;
  barrierCreate("barrier", count);
  const promises = [];
  for (let i = 0; i < count; i++) {
    promises.push(barrierAwait("barrier"));
  }
  await Promise.all(promises);
});
