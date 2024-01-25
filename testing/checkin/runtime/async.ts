// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
const {
  op_async_barrier_create,
  op_async_barrier_await,
  op_async_yield,
  op_async_throw_error_eager,
  op_async_throw_error_lazy,
  op_async_throw_error_deferred,
  op_stats_capture,
  op_stats_diff,
  op_stats_dump,
  op_stats_delete,
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

let nextStats = 0;

export class Stats {
  constructor(public name: string) {
    op_stats_capture(this.name);
  }

  dump(): StatsCollection {
    return new StatsCollection(op_stats_dump(this.name).active);
  }

  [Symbol.dispose]() {
    op_stats_delete(this.name);
  }
}

export class StatsDiff {
  #appeared;
  #disappeared;

  // deno-lint-ignore no-explicit-any
  constructor(private diff: any) {
    this.#appeared = new StatsCollection(this.diff.appeared);
    this.#disappeared = new StatsCollection(this.diff.disappeared);
  }

  get empty(): boolean {
    return this.#appeared.empty && this.#disappeared.empty;
  }

  get appeared(): StatsCollection {
    return this.#appeared;
  }

  get disappeared(): StatsCollection {
    return this.#disappeared;
  }
}

// This contains an array of serialized RuntimeActivity structs.
export class StatsCollection {
  // deno-lint-ignore no-explicit-any
  constructor(private data: any[]) {
  }

  private countResourceActivity(type: string): number {
    let count = 0;
    for (const item of this.data) {
      if (type in item) {
        count++;
      }
    }
    return count;
  }

  countOps(): number {
    return this.countResourceActivity("AsyncOp");
  }

  countResources(): number {
    return this.countResourceActivity("Resource");
  }

  countTimers(): number {
    return this.countResourceActivity("Timer") +
      this.countResourceActivity("Interval");
  }

  get empty(): boolean {
    return this.data.length == 0;
  }
}

export class StatsFactory {
  static capture(): Stats {
    return new Stats(`stats-${nextStats++}`);
  }

  static diff(before: Stats, after: Stats): StatsDiff {
    return new StatsDiff(op_stats_diff(before.name, after.name));
  }
}
