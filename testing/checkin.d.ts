// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
/// <reference path="../core/lib.deno_core.d.ts" />
// TODO(mmastrac): make this typecheck
// <reference path="../core/internal.d.ts" />
/// <reference lib="dom" />

// deno-lint-ignore-file no-explicit-any

// Types and method unavailable in TypeScript by default.
interface PromiseConstructor {
  withResolvers<T>(): {
    promise: Promise<T>;
    resolve: (value: T | PromiseLike<T>) => void;
    reject: (reason?: any) => void;
  };
}

interface ArrayBuffer {
  transfer(size: number);
}

interface SharedArrayBuffer {
  transfer(size: number);
}

declare namespace Deno {
  export function refTimer(id);
  export function unrefTimer(id);
}

declare module "ext:core/mod.js" {
  const core: any;
}

declare module "ext:core/ops" {
  function op_log_debug(...any): any;
  function op_log_info(...any): any;

  function op_test_register(...any): any;

  function op_async_throw_error_deferred(...any): any;
  function op_async_throw_error_eager(...any): any;
  function op_async_throw_error_lazy(...any): any;
  function op_error_context_async(...any): any;
  function op_error_context_sync(...any): any;
  function op_error_custom_sync(...any): any;

  function op_worker_await_close(...any): any;
  function op_worker_parent(...any): any;
  function op_worker_recv(...any): any;
  function op_worker_send(...any): any;
  function op_worker_spawn(...any): any;
  function op_worker_terminate(...any): any;
}
