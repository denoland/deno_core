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
    console.error("Failed assertion");
  }
}
