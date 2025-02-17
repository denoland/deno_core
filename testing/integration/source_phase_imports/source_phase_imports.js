// Copyright 2018-2025 the Deno authors. MIT license.
import source mod from "./add.wasm";

// To regenerate Wasm file use:
// npx -p wabt wat2wasm ./testing/integration/source_phase_imports/add.wat -o ./testing/integration/source_phase_imports/add.wasm

if (Object.getPrototypeOf(mod) !== WebAssembly.Module.prototype) {
  throw new Error("Wrong prototype");
}

if (mod[Symbol.toStringTag] !== "WebAssembly.Module") {
  throw new Error("Wrong Symbol.toStringTag");
}

console.log(mod[Symbol.toStringTag]);
