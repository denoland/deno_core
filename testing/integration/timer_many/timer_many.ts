// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
let n = 0;
for (let i = 0; i < 1e6; i++) setTimeout(() => n++, 1);
setTimeout(() => console.log(n), 2);
