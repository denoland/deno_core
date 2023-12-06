// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
import { fail, test } from "checkin:testing";

// Verify that "array by copy" proposal is enabled (https://github.com/tc39/proposal-change-array-by-copy)
test(function testArrayByCopy() {
  const a = [1, 2, 3];
  const b = a.toReversed();
  if (!(a[0] === 1 && a[1] === 2 && a[2] === 3)) {
    fail("Expected a to be intact");
  }
  if (!(b[0] === 3 && b[1] === 2 && b[2] === 1)) {
    fail("Expected b to be reversed");
  }
});

// Verify that "Array.fromAsync" proposal is enabled (https://github.com/tc39/proposal-array-from-async)
test(async function testArrayFromAsync() {
  // @ts-expect-error: Not available in TypeScript yet
  const b = await Array.fromAsync(new Map([[1, 2], [3, 4]]));
  if (b[0][0] !== 1 || b[0][1] !== 2 || b[1][0] !== 3 || b[1][1] !== 4) {
    fail("failed");
  }
});

// Verify that "Iterator helpers" proposal is enabled (https://github.com/tc39/proposal-iterator-helpers)
test(function testIteratorHelpers() {
  function* naturals() {
    let i = 0;
    while (true) {
      yield i;
      i += 1;
    }
  }

  // @ts-expect-error: Not available in TypeScript yet
  const a = naturals().take(5).toArray();
  if (a[0] !== 0 || a[1] !== 1 || a[2] !== 2 || a[3] !== 3 || a[4] !== 4) {
    fail("failed");
  }
});
