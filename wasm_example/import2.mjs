import wasmModule from "./import.wasm" with { type: "wasm" };
import bytes from "./import.wasm" with { type: "bytes" };
console.log(`hey ${exported_add}`);
console.log(`${exported_add()}`);

import {
  get_plugin_schema_version,
  get_wasm_memory_buffer_size,
} from "./plugin.wasm" with { type: "wasm" };
console.log("dprint plugin version", get_plugin_schema_version());
console.log(
  "dprint plugin get_wasm_memory_buffer_size",
  get_wasm_memory_buffer_size(),
);
