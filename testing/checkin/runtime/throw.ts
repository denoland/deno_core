// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

/**
 * This is needed to test that stack traces in extensions are correct.
 */
export function throwExceptionFromExtension() {
  innerThrowInExt();
}

function innerThrowInExt() {
  throw new Error("Failed");
}
