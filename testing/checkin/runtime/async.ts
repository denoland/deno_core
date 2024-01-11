// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
const { op_async_barrier_create, op_async_barrier_await, op_async_yield } = Deno
  .core
  .ensureFastOps();

export function barrierCreate(name: string, count: number) {
  op_async_barrier_create(name, count);
}

export async function barrierAwait(name: string) {
  await op_async_barrier_await(name);
}

export async function asyncYield() {
  await op_async_yield();
}
