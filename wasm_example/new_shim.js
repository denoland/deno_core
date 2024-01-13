import wasmMod from "./import.wasm" with { type: "wasm-module" };
import { add } from "./import_inner.mjs";
const importsObject = {};
importsObject["./import_inner.mjs"] ??= {};
importsObject["./import_inner.mjs"]["add"] = add;
const modInstance = await WebAssembly.instantiate(wasmMod, importsObject);
export const exported_add = modInstance.exports.exported_add;
