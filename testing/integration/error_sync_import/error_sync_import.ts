// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.

// This import uses a sync op that throws, and the sync op will recursively call the
// "op_apply_source_map" op because we pull its stack trace in a complex, promise-chaining
// call tree that starts with "catch_dynamic_import_promise_error". 

// We need to make sure that this doesn't trigger a recursive op error.
import "./throws.ts";
