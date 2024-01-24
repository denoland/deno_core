// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
import { barrierAwait, barrierCreate, StatsFactory } from "checkin:async";
import { assert, assertEquals, test } from "checkin:testing";

const { op_pipe_create } = Deno.core.ensureFastOps();

test(async function testStatsOps() {
  using statsBefore = StatsFactory.capture();
  assert(statsBefore.dump().empty);

  barrierCreate("barrier", 3);
  const promise1 = barrierAwait("barrier");
  assertEquals(1, StatsFactory.capture().dump().countOps());
  const promise2 = barrierAwait("barrier");
  assertEquals(2, StatsFactory.capture().dump().countOps());
  using statsMiddle = StatsFactory.capture();
  const diffMiddle = StatsFactory.diff(statsBefore, statsMiddle);
  assertEquals(0, diffMiddle.disappeared.countOps());
  assertEquals(2, diffMiddle.appeared.countOps());

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
