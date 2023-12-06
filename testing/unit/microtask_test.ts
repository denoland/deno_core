// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
import { test } from "checkin:testing";

test(async function testQueueMicrotask() {
  await new Promise((r) =>
    queueMicrotask(() => {
      console.log("In microtask!");
      r(null);
    })
  );
});
