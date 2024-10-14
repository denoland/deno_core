// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
delete globalThis.Error;

const e = new TypeError("e");
e.stack;
