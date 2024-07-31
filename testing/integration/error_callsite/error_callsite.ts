function toObj(callsite) {
  const keys = [
    "getThis",
    "getTypeName",
    "getFunction",
    "getFunctionName",
    "getMethodName",
    "getFileName",
    "getLineNumber",
    "getColumnNumber",
    "getEvalOrigin",
    "isToplevel",
    "isEval",
    "isNative",
    "isConstructor",
    "isAsync",
    "isPromiseAll",
    "getPromiseIndex",
  ];
  return Object.fromEntries(keys.map((key) => [key, callsite[key]()]));
}
Error.prepareStackTrace = function (_, callsites) {
  callsites.forEach((callsite) => {
    console.log(toObj(callsite));
    console.log(callsite.toString());
  });
  return callsites;
};

class Foo {
  constructor() {
    new Error().stack;
  }
}

new Foo();
