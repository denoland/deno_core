import "./main.js";
const { op_async_barrier_await } = Deno.core.ensureFastOps();
await op_async_barrier_await("barrier");
