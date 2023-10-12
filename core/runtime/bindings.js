// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
if (!globalThis.Deno) {
  globalThis.Deno = {
    core: {
      ops: {},
      asyncOps: {},
    },
  };

}

if (!Deno.core.encode) {
  // Deno specific builtins.
  const { fromUtf8: encode, toUtf8: decode } = globalThis;

  delete globalThis.fromUtf8;
  delete globalThis.toUtf8;
  delete globalThis.isOneByte;

  Deno.core.encode = (text) => new Uint8Array(encode(text));
  Deno.core.decode = (buffer, ignoreBOM = false) => decode(buffer, ignoreBOM);
}

Deno.__op__console = function (callConsole, console) {
  Deno.core.callConsole = callConsole;
  Deno.core.console = console;
};

Deno.__op__registerOp = function (isAsync, op, opName) {
  const core = Deno.core;
  if (isAsync) {
    if (core.ops[opName] !== undefined) {
      return;
    }
    core.asyncOps[opName] = op;
    const fn = function (...args) {
      if (this !== core.ops) {
        // deno-lint-ignore prefer-primordials
        throw new Error(
          "An async stub cannot be separated from Deno.core.ops. Use ???",
        );
      }
      return core.asyncStub(opName, args);
    };
    fn.name = opName;
    core.ops[opName] = fn;
  } else {
    core.ops[opName] = op;
  }
};

Deno.__op__unregisterOp = function (isAsync, opName) {
  if (isAsync) {
    delete Deno.core.asyncOps[opName];
  }
  delete Deno.core.ops[opName];
};

Deno.__op__cleanup = function () {
  delete Deno.__op__console;
  delete Deno.__op__registerOp;
  delete Deno.__op__unregisterOp;
  delete Deno.__op__cleanup;
};
