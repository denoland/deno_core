// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
// FAIL

function assert(cond) {
  if (!cond) {
    throw Error("assert");
  }
}
function main() {
  assert(false);
}
main();
