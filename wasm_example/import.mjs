import { exported_add } from "./import.wasm";

console.log(`exported_add: ${exported_add(4, 5)}`);

import * as dprint from "./plugin.wasm";
console.log("dprint plugin version:", dprint.get_plugin_schema_version());
console.log(
  "dprint plugin get_wasm_memory_buffer_size:",
  dprint.get_wasm_memory_buffer_size(),
);
