// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.

/**
 * This is needed to test that stack traces in extensions are correct.
 */
export function throwInExt() {
  innertTrowInExt();
}

function innertTrowInExt() {
  throw new Error("Failed");
}
