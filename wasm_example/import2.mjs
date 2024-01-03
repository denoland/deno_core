import wasmMod from "./import.wasm" with { type: "wasm" };
console.log(`hey ${wasmMod.exports.exported_add}`);
console.log(`${wasmMod.exports.exported_add()}`);
