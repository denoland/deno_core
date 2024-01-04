// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
import { test } from "checkin:testing";
const {
  op_async_void_deferred,
} = Deno.core.ensureFastOps();

test(async function testAsyncOp() {
  await op_async_void_deferred();
});
