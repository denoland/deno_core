// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
console.log("should not panic");
await new Promise((r) => {
  const id = setTimeout(r, 1000);
  Deno.unrefTimer(id);
});
console.log("didn't panic!");
