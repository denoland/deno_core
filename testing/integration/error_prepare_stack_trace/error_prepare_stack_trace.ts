// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
let errorCallsitePrototype;
Error.prepareStackTrace = (err, frames) => {
  return frames.map((frame) => {
    errorCallsitePrototype = Object.getPrototypeOf(frame);
    console.log(Object.getOwnPropertyNames(Object.getPrototypeOf(frame)));
    console.log(Object.getOwnPropertyNames(frame));
    return frame.toString();
  });
};

console.log(new Error("fail").stack);


for (const prop of Object.getOwnPropertyNames(errorCallsitePrototype)) {
  if (typeof errorCallsitePrototype[prop] === "function") {
    let error;
    try {
      errorCallsitePrototype[prop]();
    } catch (e) {
      error = e;
    }
    if (error) {
      console.log(`${prop}() threw an error: ${error.message}`);
    } else {
      console.log(`${prop}() did not throw an error`);
    }
  }
}
