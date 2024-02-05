// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
import { exported_add } from "./add.wasm";

console.log(`exported_add: ${exported_add(4, 5)}`);
