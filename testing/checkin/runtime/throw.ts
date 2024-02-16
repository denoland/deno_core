// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.

/**
 * This is needed to test that stack traces in extensions are correct.
 */
export function throwInExt() {
  innerThrowInExt();
}

function innerThrowInExt() {
  throw new Error("Failed");
}
