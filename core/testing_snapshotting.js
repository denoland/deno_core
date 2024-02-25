console.log("This gets executed");

globalThis.foo = "foo";
globalThis.testingSnapshot = function () {
  console.log("Hello from testing_snapshotting.js!");
};
