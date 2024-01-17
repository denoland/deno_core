// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.

Deno.__op__registerOp = function (isAsync, op, opName) {
  const core = Deno.core;
  if (isAsync) {
    // TODO(bartlomieju): this is fishy and suggest an op can be registered
    // more than once. Maybe it should be an assertion? And also why is it
    // only for async ops?
    if (core.ops[opName] !== undefined) {
      return;
    }
    core.asyncOps[opName] = op;
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
  delete Deno.__op__registerOp;
  delete Deno.__op__unregisterOp;
  delete Deno.__op__cleanup;
};
