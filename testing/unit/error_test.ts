// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
import { throwCustomError } from "checkin:error";
import { assert, assertEquals, test } from "checkin:testing";

test(function testCustomError() {
  try {
    throwCustomError("uh oh");
  } catch (e) {
    assertEquals(e.message, "uh oh");
    assert(e instanceof Deno.core.BadResource);
  }
});
