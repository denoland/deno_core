import wasmMod from "./import.wasm" with { type: "wasm" };
console.log(`hey ${wasmMod.exports.exported_add}`);
console.log(`${wasmMod.exports.exported_add()}`);

import dprintPlugin from "./plugin.wasm" with { type: "wasm" };
console.log(`hey ${Object.keys(dprintPlugin.exports)}`);
console.log(
  "dprint plugin version",
  dprintPlugin.exports.get_plugin_schema_version(),
);
console.log(
  "dprint plugin get_wasm_memory_buffer_size",
  dprintPlugin.exports.get_wasm_memory_buffer_size(),
);
