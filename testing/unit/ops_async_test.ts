// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
import { assertEquals, test } from "checkin:testing";
import { asyncYield, barrierAwait, barrierCreate } from "checkin:async";
import { asyncThrow } from "checkin:error";

function assertStackTraceEquals(stack1: string, stack2: string) {
  function normalize(s: string) {
    return s.replace(/[ ]+/g, " ")
      .replace(/^ /g, "")
      .replace(/\d+:\d+/g, "line:col")
      .trim();
  }

  assertEquals(normalize(stack1), normalize(stack2));
}

// Test that stack traces from async ops are all sane
test(async function testAsyncThrow() {
  try {
    await asyncThrow("eager");
  } catch (e) {
    assertStackTraceEquals(
      e.stack,
      `TypeError: Error
        at asyncThrow (checkin:error:line:col)
        at testAsyncThrow (test:///unit/ops_async_test.ts:line:col)
      `,
    );
  }
  try {
    await asyncThrow("lazy");
  } catch (e) {
    assertStackTraceEquals(
      e.stack,
      `TypeError: Error
        at async asyncThrow (checkin:error:line:col)
        at async testAsyncThrow (test:///unit/ops_async_test.ts:line:col)
      `,
    );
  }
  try {
    await asyncThrow("deferred");
  } catch (e) {
    assertStackTraceEquals(
      e.stack,
      `TypeError: Error
        at async asyncThrow (checkin:error:line:col)
        at async testAsyncThrow (test:///unit/ops_async_test.ts:line:col)`,
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

test(async function testAsyncOpCallTrace() {
  const oldTracingState = Deno.core.isOpCallTracingEnabled();
  Deno.core.setOpCallTracingEnabled(true);
  barrierCreate("barrier", 2);
  try {
    let p1 = barrierAwait("barrier");
    assertStackTraceEquals(
      Deno.core.getOpCallTraceForPromise(p1),
      `
      at handleOpCallTracing (ext:core/00_infra.js:line:col)
      at op_async_barrier_await (ext:core/00_infra.js:line:col)
      at barrierAwait (checkin:async:line:col)
      at testAsyncOpCallTrace (test:///unit/ops_async_test.ts:line:col)
    `,
    );
    let p2 = barrierAwait("barrier");
    await Promise.all([p1, p2]);
  } finally {
    Deno.core.setOpCallTracingEnabled(oldTracingState);
  }
});
