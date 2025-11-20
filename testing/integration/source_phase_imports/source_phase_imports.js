// Copyright 2018-2025 the Deno authors. MIT license.
import source foo from "../wasm_imports/add.wasm";
console.log(foo);
import source bar from "./other.js";
console.log(bar);
