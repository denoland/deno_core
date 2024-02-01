import {
  throwCustomError,
  throwErrorWithContextAsync,
  throwErrorWithContextSync,
} from "checkin:error";
import { assert, assertEquals, test } from "checkin:testing";

test(function testSyncContext() {
  try {
    throwErrorWithContextSync("message", "with context");
  } catch (e) {
    assertEquals(e.message, "with context: message");
  }
});

test(async function testAsyncContext() {
  try {
    await throwErrorWithContextAsync("message", "with context");
  } catch (e) {
    assertEquals(e.message, "with context: message");
  }
});

test(function testCustomError() {
  try {
    throwCustomError("uh oh");
  } catch (e) {
    assertEquals(e.message, "uh oh");
    assert(e instanceof Deno.core.BadResource);
  }
});
