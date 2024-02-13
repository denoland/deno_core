// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
import { barrierAwait, barrierCreate, StatsFactory } from "checkin:async";
import {
  assert,
  assertEquals,
  assertStackTraceEquals,
  test,
} from "checkin:testing";

const { op_pipe_create } = Deno.core.ensureFastOps();

test(async function testStatsOps() {
  using statsBefore = StatsFactory.capture();
  assert(statsBefore.dump().empty);

  barrierCreate("barrier", 3);
  const promise1 = barrierAwait("barrier");
  assertEquals(1, StatsFactory.capture().dump().countOps());
  const promise2 = barrierAwait("barrier");
  assertEquals(2, StatsFactory.capture().dump().countOps());
  // No traces here at all, even though we have ops
  assertEquals(0, StatsFactory.capture().dump().countOpsWithTraces());
  using statsMiddle = StatsFactory.capture();
  const diffMiddle = StatsFactory.diff(statsBefore, statsMiddle);
  assertEquals(0, diffMiddle.disappeared.countOps());
  assertEquals(2, diffMiddle.appeared.countOps());
  // No traces here at all, even though we have ops
  assertEquals(0, diffMiddle.appeared.countOpsWithTraces());

  await Promise.all([promise1, promise2, barrierAwait("barrier")]);

  using statsAfter = StatsFactory.capture();
  const diff = StatsFactory.diff(statsBefore, statsAfter);
  assert(diff.empty);
});

test(function testStatsResources() {
  using statsBefore = StatsFactory.capture();

  const [p1, p2] = op_pipe_create();
  using statsMiddle = StatsFactory.capture();
  const diffMiddle = StatsFactory.diff(statsBefore, statsMiddle);
  assertEquals(0, diffMiddle.disappeared.countResources());
  assertEquals(2, diffMiddle.appeared.countResources());
  Deno.core.close(p1);
  Deno.core.close(p2);

  using statsAfter = StatsFactory.capture();
  const diff = StatsFactory.diff(statsBefore, statsAfter);
  assert(diff.empty);
});

test(function testTimers() {
  using statsBefore = StatsFactory.capture();

  const timeout = setTimeout(() => null, 1000);
  const interval = setInterval(() => null, 1000);

  using statsMiddle = StatsFactory.capture();
  const diffMiddle = StatsFactory.diff(statsBefore, statsMiddle);
  assertEquals(0, diffMiddle.disappeared.countTimers());
  assertEquals(2, diffMiddle.appeared.countTimers());
  clearTimeout(timeout);
  clearInterval(interval);

  using statsAfter = StatsFactory.capture();
  const diff = StatsFactory.diff(statsBefore, statsAfter);
  assert(diff.empty);
});

test(async function testAsyncOpCallTrace() {
  const oldTracingState = Deno.core.isOpCallTracingEnabled();
  Deno.core.setOpCallTracingEnabled(true);
  barrierCreate("barrier", 2);
  try {
    const tracesBefore = Deno.core.getAllOpCallTraces();
    using statsBefore = StatsFactory.capture();
    const p1 = barrierAwait("barrier");
    const tracesAfter = Deno.core.getAllOpCallTraces();
    using statsAfter = StatsFactory.capture();
    const diff = StatsFactory.diff(statsBefore, statsAfter);
    // We don't test the contents, just that we have a trace here
    assertEquals(diff.appeared.countOpsWithTraces(), 1);

    assertEquals(tracesAfter.size, tracesBefore.size + 1);
    assertStackTraceEquals(
      Deno.core.getOpCallTraceForPromise(p1),
      `
      at op_async_barrier_await (ext:core/00_infra.js:line:col)
      at barrierAwait (checkin:async:line:col)
      at testAsyncOpCallTrace (test:///unit/stats_test.ts:line:col)
    `,
    );
    const p2 = barrierAwait("barrier");
    await Promise.all([p1, p2]);
  } finally {
    Deno.core.setOpCallTracingEnabled(oldTracingState);
  }
});
