// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
import { barrierAwait, barrierCreate, StatsFactory } from "checkin:async";
import { assert, assertEquals, test } from "checkin:testing";

test(async function testStatsOps() {
  using statsBefore = StatsFactory.capture();
  assert(statsBefore.dump().empty);

  barrierCreate("barrier", 3);
  let promise1 = barrierAwait("barrier");
  assertEquals(1, StatsFactory.capture().dump().countOps());
  let promise2 = barrierAwait("barrier");
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
