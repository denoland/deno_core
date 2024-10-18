// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

const { op_import_sync, op_path_to_url } = Deno.core.ops;

const resolve = (p: string) => op_path_to_url(p);

await import(resolve("./integration/import_sync/sync.js"));
console.log(op_import_sync(resolve("./integration/import_sync/sync.js")));
