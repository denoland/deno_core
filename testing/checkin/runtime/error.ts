// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
import {
  op_async_throw_error_deferred,
  op_async_throw_error_eager,
  op_async_throw_error_lazy,
  op_error_context_async,
  op_error_context_sync,
  op_error_custom_sync,
} from "ext:core/ops";

export async function asyncThrow(kind: "lazy" | "eager" | "deferred") {
  const op = {
    lazy: op_async_throw_error_lazy,
    eager: op_async_throw_error_eager,
    deferred: op_async_throw_error_deferred,
  }[kind];
  return await op();
}

export function throwCustomError(message: string) {
  op_error_custom_sync(message);
}

export function throwErrorWithContextSync(message: string, context: string) {
  op_error_context_sync(message, context);
}

export async function throwErrorWithContextAsync(
  message: string,
  context: string,
) {
  await op_error_context_async(message, context);
}
