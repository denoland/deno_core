// Copyright 2018-2025 the Deno authors. MIT license.
console.log("deferred module evaluated");
export const value = 42;
export function add(a, b) {
  return a + b;
}
