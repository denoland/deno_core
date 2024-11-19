// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
import { exported_add } from "./add.wasm";

// To regenerate Wasm file use:
// npx -p wabt wat2wasm ./testing/integration/wasm_imports/add.wat -o ./testing/integration/wasm_imports/add.wasm

console.log(`exported_add: ${exported_add(4, 5)}`);
