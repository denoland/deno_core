// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

// deno-lint-ignore no-explicit-any
(Error as any).prepareStackTrace = (_err: unknown, frames: any[]) => {
  return frames.map((frame) => frame.getFileName());
};

new Promise((_, reject) => {
  reject(new Error("fail").stack);
}).catch((err) => {
  console.log(err);
});
