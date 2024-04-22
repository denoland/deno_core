// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
import { assert, fail, test } from "checkin:testing";

const { op_task_submit } = Deno.core.ops;

test(async function testTaskSubmit() {
  let { promise, resolve } = Promise.withResolvers();
  op_task_submit(() => {
    resolve(undefined);
  });
  await promise;
});
