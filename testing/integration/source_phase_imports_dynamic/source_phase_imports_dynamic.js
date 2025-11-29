// Copyright 2018-2025 the Deno authors. MIT license.
const foo = await import.source("../wasm_imports/add.wasm");
console.log(foo instanceof WebAssembly.Module);
