#!/usr/bin/env deno run --allow-read --allow-write
// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.

const doNotModify =
  "/* DO NOT MODIFY: use rebuild_async_stubs.js to regenerate */\n";

// The template function we build opAsync and op_async_N functions from
function __TEMPLATE__(__ARGS_PARAM__) {
  const id = (nextPromiseId + 1) & 0xffffffff;
  try {
    const maybeResult = __OP__(__ARGS__);
    if (maybeResult !== undefined) {
      return PromiseResolve(maybeResult);
    }
  } catch (err) {
    __ERR__;
    ErrorCaptureStackTrace(err, __TEMPLATE__);
    return PromiseReject(err);
  }
  nextPromiseId = id;
  let promise = PromisePrototypeThen(
    setPromise(id),
    unwrapOpError(eventLoopTick),
  );
  promise = handleOpCallTracing(opName, id, promise);
  promise[promiseIdSymbol] = id;
  return promise;
}

const coreJsPath = new URL("01_core.js", import.meta.url).pathname;
const coreJs = Deno.readTextFileSync(coreJsPath);

const corePristine = coreJs.replaceAll(
  /\/\* BEGIN TEMPLATE ([^ ]+) \*\/.*?\/\* END TEMPLATE \*\//smg,
  "TEMPLATE-$1",
);
const templateString = __TEMPLATE__.toString();
let asyncStubCases = "/* BEGIN TEMPLATE setUpAsyncStub */\n";
asyncStubCases += doNotModify;
const vars = "abcdefghijklm";
for (let i = 0; i < 10; i++) {
  let args = "id";
  for (let j = 0; j < i; j++) {
    args += `, ${vars[j]}`;
  }
  const name = `async_op_${i}`;
  // Replace the name and args, and add a two-space indent
  const func = `fn = ${templateString}`
    .replaceAll(/__TEMPLATE__/g, name)
    .replaceAll(/__ARGS__/g, args)
    .replaceAll(/__ARGS_PARAM__/g, args.replace(/id(, )?/, ""))
    .replaceAll(/__OP__/g, "originalOp")
    .replaceAll(/[\s]*__ERR__;/g, "")
    .replaceAll(/^/gm, "  ");
  asyncStubCases += `
case ${i}:
${func};
  break;
  `.trim() + "\n";
}
asyncStubCases += "/* END TEMPLATE */";

const opAsync = "/* BEGIN TEMPLATE opAsync */\n" + doNotModify +
  templateString
    .replaceAll(/__TEMPLATE__/g, "opAsync")
    .replaceAll(/__ARGS_PARAM__/g, "opName, ...args")
    .replaceAll(/__ARGS__/g, "id, ...new SafeArrayIterator(args)")
    .replaceAll(/__OP__/g, "asyncOps[opName]")
    .replaceAll(
      /__ERR__;/g,
      "if (!ReflectHas(asyncOps, opName)) {\n      return PromiseReject(new TypeError(`${opName} is not a registered op`));\n    }",
    ) +
  "\n/* END TEMPLATE */";

const asyncStubIndent =
  corePristine.match(/^([\t ]+)(?=TEMPLATE-setUpAsyncStub)/m)[0];
const opAsyncIndent = corePristine.match(/^([\t ]+)(?=TEMPLATE-opAsync)/m)[0];

const coreOutput = corePristine
  .replace(
    /[\t ]+TEMPLATE-setUpAsyncStub/,
    asyncStubCases.replaceAll(/^/gm, asyncStubIndent),
  )
  .replace(/[\t ]+TEMPLATE-opAsync/, opAsync.replaceAll(/^/gm, opAsyncIndent));

Deno.writeTextFileSync(coreJsPath, coreOutput);
