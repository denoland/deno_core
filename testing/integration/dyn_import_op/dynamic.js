const { op_async_yield } = Deno.core.ensureFastOps();
await op_async_yield();
