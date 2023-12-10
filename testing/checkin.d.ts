// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
/// <reference path="../core/lib.deno_core.d.ts" />
/// <reference lib="dom" />

// Types and method unavailable in TypeScript by default.
interface PromiseConstructor {
  withResolvers<T>(): {
    promise: Promise<T>;
    resolve: (value: T | PromiseLike<T>) => void;
    // deno-lint-ignore no-explicit-any
    reject: (reason?: any) => void;
  };
}
