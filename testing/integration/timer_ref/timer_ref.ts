// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
setTimeout(() => console.log("a"), 1000);
setTimeout(() => console.log("b"), 2000);
const c = setTimeout(() => console.log("c"), 30_000);
Deno.core.unrefTimer(c);
