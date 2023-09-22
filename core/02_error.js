// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
"use strict";

((window) => {
  const core = Deno.core;
  const ops = core.ops;
  const {
    Error,
    ObjectDefineProperties,
    ArrayPrototypePush,
    StringPrototypeStartsWith,
    StringPrototypeEndsWith,
    Uint8Array,
    Uint32Array,
  } = window.__bootstrap.primordials;

  const DATA_URL_ABBREV_THRESHOLD = 150; // keep in sync with ./error.rs

  // Keep in sync with `format_file_name` in ./error.rs
  function formatFileName(fileName) {
    if (
      fileName.startsWith("data:") &&
      fileName.length > DATA_URL_ABBREV_THRESHOLD
    ) {
      return ops.op_format_file_name(fileName);
    }
    return fileName;
  }

  // Keep in sync with `cli/fmt_errors.rs`.
  function formatLocation(callSite) {
    if (callSite.isNative) {
      return "native";
    }
    let result = "";
    if (callSite.fileName) {
      result += formatFileName(callSite.fileName);
    } else {
      if (callSite.isEval) {
        if (callSite.evalOrigin == null) {
          throw new Error("assert evalOrigin");
        }
        result += `${callSite.evalOrigin}, `;
      }
      result += "<anonymous>";
    }
    if (callSite.lineNumber !== null) {
      result += `:${callSite.lineNumber}`;
      if (callSite.columnNumber !== null) {
        result += `:${callSite.columnNumber}`;
      }
    }
    return result;
  }

  // Keep in sync with `cli/fmt_errors.rs`.
  function formatCallSiteEval(callSite) {
    let result = "";
    if (callSite.isAsync) {
      result += "async ";
    }
    if (callSite.isPromiseAll) {
      result += `Promise.all (index ${callSite.promiseIndex})`;
      return result;
    }
    const isMethodCall = !(callSite.isToplevel || callSite.isConstructor);
    if (isMethodCall) {
      if (callSite.functionName) {
        if (callSite.typeName) {
          if (
            !StringPrototypeStartsWith(callSite.functionName, callSite.typeName)
          ) {
            result += `${callSite.typeName}.`;
          }
        }
        result += callSite.functionName;
        if (callSite.methodName) {
          if (
            !StringPrototypeEndsWith(callSite.functionName, callSite.methodName)
          ) {
            result += ` [as ${callSite.methodName}]`;
          }
        }
      } else {
        if (callSite.typeName) {
          result += `${callSite.typeName}.`;
        }
        if (callSite.methodName) {
          result += callSite.methodName;
        } else {
          result += "<anonymous>";
        }
      }
    } else if (callSite.isConstructor) {
      result += "new ";
      if (callSite.functionName) {
        result += callSite.functionName;
      } else {
        result += "<anonymous>";
      }
    } else if (callSite.functionName) {
      result += callSite.functionName;
    } else {
      result += formatLocation(callSite);
      return result;
    }

    result += ` (${formatLocation(callSite)})`;
    return result;
  }

  const applySourceMapRetBuf = new Uint32Array(2);
  const applySourceMapRetBufView = new Uint8Array(applySourceMapRetBuf.buffer);

  function prepareStackTrace(error, callSites) {
    const message = error.message !== undefined ? error.message : "";
    const name = error.name !== undefined ? error.name : "Error";
    let stack;
    if (name != "" && message != "") {
      stack = `${name}: ${message}`;
    } else if ((name || message) != "") {
      stack = name || message;
    } else {
      stack = "";
    }
    ObjectDefineProperties(error, {
      __callSiteEvals: { __proto__: null, value: [], configurable: true },
    });
    for (let i = 0; i < callSites.length; ++i) {
      const v8CallSite = callSites[i];
      const callSite = {
        this: v8CallSite.getThis(),
        typeName: v8CallSite.getTypeName(),
        function: v8CallSite.getFunction(),
        functionName: v8CallSite.getFunctionName(),
        methodName: v8CallSite.getMethodName(),
        fileName: v8CallSite.getFileName(),
        lineNumber: v8CallSite.getLineNumber(),
        columnNumber: v8CallSite.getColumnNumber(),
        evalOrigin: v8CallSite.getEvalOrigin(),
        isToplevel: v8CallSite.isToplevel(),
        isEval: v8CallSite.isEval(),
        isNative: v8CallSite.isNative(),
        isConstructor: v8CallSite.isConstructor(),
        isAsync: v8CallSite.isAsync(),
        isPromiseAll: v8CallSite.isPromiseAll(),
        promiseIndex: v8CallSite.getPromiseIndex(),
      };
      let res = 0;
      if (
        callSite.fileName !== null && callSite.lineNumber !== null &&
        callSite.columnNumber !== null
      ) {
        res = ops.op_apply_source_map(
          callSite.fileName,
          callSite.lineNumber,
          callSite.columnNumber,
          applySourceMapRetBufView,
        );
      }
      if (res >= 1) {
        callSite.lineNumber = applySourceMapRetBuf[0];
        callSite.columnNumber = applySourceMapRetBuf[1];
      }
      if (res >= 2) {
        callSite.fileName = ops.op_apply_source_map_filename();
      }
      ArrayPrototypePush(error.__callSiteEvals, callSite);
      stack += `\n    at ${formatCallSiteEval(callSite)}`;
    }
    return stack;
  }

  Error.prepareStackTrace = prepareStackTrace;
})(this);
