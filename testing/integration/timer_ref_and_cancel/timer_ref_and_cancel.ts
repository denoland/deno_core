// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
setTimeout(() => console.log("a"), 1000);
setTimeout(() => console.log("b"), 2000);
const c = setTimeout(() => console.log("c"), 3000);
const d = setTimeout(() => console.log("d"), 4000);
// c should not run, if d is cancelled
Deno.unrefTimer(c);
clearTimeout(d);
