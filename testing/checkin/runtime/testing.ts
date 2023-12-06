// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
/// <reference path="../../../core/lib.deno_core.d.ts" />

const {
  op_test_register,
} = Deno.core.ensureFastOps();

/**
 * Define a sync or async test function.
 */
export function test(test: () => void | Promise<void>) {
  op_test_register(test.name, test);
}

/**
 * Assert a value is truthy.
 */
export function assert(value) {
  if (!value) {
    console.debug("Failed assertion");
    throw new Error("Failed assertion");
  }
}

/**
 * Assert two values match (==).
 */
export function assertEquals(a1, a2) {
  assert(a1 == a2);
}

/**
 * Assert two arrays match (==).
 */
export function assertArrayEquals(a1, a2) {
  assertEquals(a1.length, a2.length);

  for (const index in a1) {
    assertEquals(a1[index], a2[index]);
  }
}
