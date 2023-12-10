// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
import { assert, assertEquals, test } from "checkin:testing";

test(async function testTimeout() {
  const { promise, resolve } = Promise.withResolvers();
  const id = setTimeout(() => {
    resolve(null);
  }, 1);
  assert(id > 0);
  await promise;
});

test(async function testTimeoutWithPromise() {
  await new Promise((r) => setTimeout(r, 1));
});

test(async function testManyTimers() {
  let n = 0;
  for (let i = 0; i < 1e6; i++) {
    setTimeout(() => n++, 1);
  }
  await new Promise((r) => setTimeout(r, 2));
  assertEquals(n, 1e6);
});

test(async function testManyIntervals() {
  const expected = 1000;
  let n = 0;
  for (let i = 0; i < 100; i++) {
    let count = 0;
    const id = setInterval(() => {
      if (count++ == 10) {
        clearInterval(id);
      } else {
        n++;
      }
      assert(n <= expected, `${n} <= ${expected}`);
    }, 1);
  }
  let id: number;
  await new Promise((r) =>
    id = setInterval(() => {
      assert(n <= expected, `${n} <= ${expected}`);
      if (n == expected) {
        clearInterval(id);
        r(null);
      }
    }, 1)
  );
  assertEquals(n, expected);
});

test(async function testTimerDepth() {
  const { promise, resolve } = Promise.withResolvers();
  assertEquals(Deno.core.getTimerDepth(), 0);
  setTimeout(() => {
    assertEquals(Deno.core.getTimerDepth(), 1);
    setTimeout(() => {
      assertEquals(Deno.core.getTimerDepth(), 2);
      resolve(null);
    }, 1);
  }, 1);
  await promise;
});
