import {
  throwErrorWithContextAsync,
  throwErrorWithContextSync,
} from "checkin:error";
import { assertEquals, test } from "checkin:testing";

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
