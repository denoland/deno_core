import wasmBytes from "./import.wasm" with { type: "bytes" };
import { wasmShim } from "./test.js";

const wasmMod = await wasmShim(wasmBytes);
console.log(`hey ${wasmMod.exports.exported_add}`);
console.log(`${wasmMod.exports.exported_add()}`);
