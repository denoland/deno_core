// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
import { assertEquals, test } from "checkin:testing";
import {
  asyncThrow,
  asyncYield,
  barrierAwait,
  barrierCreate,
} from "checkin:async";

// Test that stack traces from async ops are all sane
test(async function testAsyncThrow() {
  try {
    await asyncThrow("eager");
  } catch (e) {
    const stack = e.stack.replace(/\d+:\d+/g, "line:col");
    assertEquals(
      stack,
      "TypeError: Error\n    at asyncThrow (checkin:async:line:col)\n    at testAsyncThrow (test:///unit/ops_async_test.ts:line:col)",
    );
  }
  try {
    await asyncThrow("lazy");
  } catch (e) {
    const stack = e.stack.replace(/\d+:\d+/g, "line:col");
    assertEquals(
      stack,
      "TypeError: Error\n    at async asyncThrow (checkin:async:line:col)\n    at async testAsyncThrow (test:///unit/ops_async_test.ts:line:col)",
    );
  }
  try {
    await asyncThrow("deferred");
  } catch (e) {
    const stack = e.stack.replace(/\d+:\d+/g, "line:col");
    assertEquals(
      stack,
      "TypeError: Error\n    at async asyncThrow (checkin:async:line:col)\n    at async testAsyncThrow (test:///unit/ops_async_test.ts:line:col)",
    );
  }
});

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
