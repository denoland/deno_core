// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
// FAIL

function foo() {
  eval(`eval("eval('throw new Error()')")`);
}

foo();