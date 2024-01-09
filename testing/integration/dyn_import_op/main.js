const { op_async_barrier_create, op_async_barrier_await } = Deno.core
  .ensureFastOps();
op_async_barrier_create("barrier", 2);
(async () => {
  await import("./dynamic.js");
  await op_async_barrier_await("barrier");
})();
