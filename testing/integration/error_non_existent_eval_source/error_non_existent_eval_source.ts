// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
const AsyncFunction = Object.getPrototypeOf(async function () {
  // empty
}).constructor;

const func = new AsyncFunction(
  `return doesNotExist();
    //# sourceURL=empty.eval`,
);

func.call({});
