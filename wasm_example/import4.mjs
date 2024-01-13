import { exported_add } from "./import.wasm" with { type: "wasm2" };
// import { exported_add } from "./new_shim.js";

console.log(`hey ${exported_add(4, 5)}`);

// import dprintPlugin from "./plugin.wasm" with { type: "wasm-module" };
// console.log(
//   `hey ${JSON.stringify(WebAssembly.Module.exports(dprintPlugin))}`,
// );
// console.log(
//   `hey ${JSON.stringify(WebAssembly.Module.imports(dprintPlugin))}`,
// );
