/*
`./target/release/examples/fs_module_loader ./promise_hooks.js`

```
# Benchmark: async function calls per second.

Baseline                      : 26_178_010 op/s   
No-op promise hooks installed : 12_091_898 op/s  ( 2.16 times slower)
AsyncLocalStorage[1]          :  2_020_202 op/s  (12.95 times slower)

[1] Just keeping track of the async context; the store itself is never accessed.
```
*/

async function afn() {}

function noop() {}

async function bench() {
  const OPS = 1e7;
  const start = Date.now();
  for (let j = 0; j < OPS; j++) {
    const p = afn();
    await p;
  }
  const elapsed = Date.now() - start;
  const secs = elapsed / 1000;
  const opsPerSec = OPS / secs;
  Deno.core.print(`Iterations per second: ${opsPerSec.toFixed(0)}\n`);
  return opsPerSec;
}

async function benches() {
  const ROUNDS = 3;
  const results = [];
  for (let i = 0; i < ROUNDS; i++) {
    results.push(await bench())
  }
  return results;
}

let invoke = f => f();

// Uncomment to install no-op promise hooks.
// Deno.core.setPromiseHooks(noop, noop, noop, noop);

// Uncomment to install AsyncLocalStorage promise hooks.
import { AsyncLocalStorage } from "./async_hooks.js";
invoke = f => (new AsyncLocalStorage()).run({}, f);

const results = await invoke(benches);
