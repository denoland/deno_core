// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
import { assert, fail, test } from "checkin:testing";

const { op_task_submit } = Deno.core.ops;

test(async function testTaskSubmit1() {
  let { promise, resolve } = Promise.withResolvers();
  op_task_submit(() => {
    resolve(undefined);
  });
  await promise;
});

test(async function testTaskSubmit2() {
  for (let i = 0; i < 2; i++) {
    let { promise, resolve } = Promise.withResolvers();
    op_task_submit(() => {
      resolve(undefined);
    });
    await promise;
  }
});

test(async function testTaskSubmit3() {
  for (let i = 0; i < 3; i++) {
    let { promise, resolve } = Promise.withResolvers();
    op_task_submit(() => {
      resolve(undefined);
    });
    await promise;
  }
});
