// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
// https://github.com/denoland/deno_core/issues/743
console.log("1");
Object.defineProperty(Promise.prototype, "constructor", {
  get() {
    throw "x";
  },
});
console.log("2");
