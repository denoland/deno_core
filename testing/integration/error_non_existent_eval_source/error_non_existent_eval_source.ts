const AsyncFunction = Object.getPrototypeOf(async function () {
  // empty
}).constructor;

const func = new AsyncFunction(
  `return doesNotExist();
    //# sourceURL=empty.eval`,
);

func.call({});
