// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
const {
  op_async_barrier_create,
  op_async_barrier_await,
  op_async_yield,
  op_async_throw_error_eager,
  op_async_throw_error_lazy,
  op_async_throw_error_deferred,
} = Deno
  .core
  .ensureFastOps();

export async function asyncThrow(kind: "lazy" | "eager" | "deferred") {
  const op = {
    lazy: op_async_throw_error_lazy,
    eager: op_async_throw_error_eager,
    deferred: op_async_throw_error_deferred,
  }[kind];
  return await op();
}

export function barrierCreate(name: string, count: number) {
  op_async_barrier_create(name, count);
}

export async function barrierAwait(name: string) {
  await op_async_barrier_await(name);
}

export async function asyncYield() {
  await op_async_yield();
}
