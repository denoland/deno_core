const foo = await import.source("../wasm_imports/add.wasm");
console.log(foo instanceof WebAssembly.Module);
const bar = await import.source("./other.js");
// TODO(nayeemrmn): Change to `console.log(bar instanceof ModuleSource);` when
// ModuleSource is defined:
// https://github.com/tc39/proposal-source-phase-imports#js-module-source
console.log(typeof bar == "object");
