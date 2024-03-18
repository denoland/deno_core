// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
// The handler is set up before this main module is executed
globalThis.onmainmodule = (main) => {
  console.log(main);
};

export const b = 2;

export default {
  a: 1,
};
