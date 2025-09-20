// Copyright 2018-2025 the Deno authors. MIT license.
import { assert, test } from "checkin:testing";
import { primordials } from "checkin:primordials";

const { SymbolAsyncDispose, SymbolDispose } = primordials;

test(function testDisposeSymbol() {
  assert(typeof SymbolAsyncDispose === "symbol");
  assert(typeof SymbolDispose === "symbol");

  assert(Symbol.asyncDispose === SymbolAsyncDispose);
  assert(Symbol.dispose === SymbolDispose);
});
