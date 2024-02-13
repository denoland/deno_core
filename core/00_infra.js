// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
"use strict";

((window) => {
  const {
    Array,
    ArrayPrototypeFill,
    Error,
    ErrorCaptureStackTrace,
    MapPrototypeDelete,
    MapPrototypeGet,
    MapPrototypeHas,
    MapPrototypeSet,
    ObjectAssign,
    ObjectDefineProperty,
    ObjectFreeze,
    Promise,
    PromiseReject,
    PromiseResolve,
    PromisePrototypeThen,
    RangeError,
    ReferenceError,
    SafeMap,
    StringPrototypeSplit,
    SymbolFor,
    SyntaxError,
    TypeError,
    URIError,
  } = window.__bootstrap.primordials;

  let nextPromiseId = 0;
  const promiseMap = new SafeMap();
  const RING_SIZE = 4 * 1024;
  const NO_PROMISE = null; // Alias to null is faster than plain nulls
  const promiseRing = ArrayPrototypeFill(new Array(RING_SIZE), NO_PROMISE);
  // TODO(bartlomieju): it future use `v8::Private` so it's not visible
  // to users. Currently missing bindings.
  const promiseIdSymbol = SymbolFor("Deno.core.internalPromiseId");

  let isOpCallTracingEnabled = false;
  let submitOpCallTrace;
  let eventLoopTick;

  function __setOpCallTracingEnabled(enabled) {
    isOpCallTracingEnabled = enabled;
  }

  function __initializeCoreMethods(eventLoopTick_, submitOpCallTrace_) {
    eventLoopTick = eventLoopTick_;
    submitOpCallTrace = submitOpCallTrace_;
  }

  const build = {
    target: "unknown",
    arch: "unknown",
    os: "unknown",
    vendor: "unknown",
    env: undefined,
  };

  function setBuildInfo(target) {
    const { 0: arch, 1: vendor, 2: os, 3: env } = StringPrototypeSplit(
      target,
      "-",
      4,
    );
    build.target = target;
    build.arch = arch;
    build.vendor = vendor;
    build.os = os;
    build.env = env;
    ObjectFreeze(build);
  }

  const errorMap = {};
  // Builtin v8 / JS errors
  registerErrorClass("Error", Error);
  registerErrorClass("RangeError", RangeError);
  registerErrorClass("ReferenceError", ReferenceError);
  registerErrorClass("SyntaxError", SyntaxError);
  registerErrorClass("TypeError", TypeError);
  registerErrorClass("URIError", URIError);

  function buildCustomError(className, message, code) {
    let error;
    try {
      error = errorMap[className]?.(message);
    } catch (e) {
      throw new Error(
        `Unable to build custom error for "${className}"\n  ${e.message}`,
      );
    }
    // Strip buildCustomError() calls from stack trace
    if (typeof error == "object") {
      ErrorCaptureStackTrace(error, buildCustomError);
      if (code) {
        error.code = code;
      }
    }
    return error;
  }

  function registerErrorClass(className, errorClass) {
    registerErrorBuilder(className, (msg) => new errorClass(msg));
  }

  function registerErrorBuilder(className, errorBuilder) {
    if (typeof errorMap[className] !== "undefined") {
      throw new TypeError(`Error class for "${className}" already registered`);
    }
    errorMap[className] = errorBuilder;
  }

  function setPromise(promiseId) {
    const idx = promiseId % RING_SIZE;
    // Move old promise from ring to map
    const oldPromise = promiseRing[idx];
    if (oldPromise !== NO_PROMISE) {
      const oldPromiseId = promiseId - RING_SIZE;
      MapPrototypeSet(promiseMap, oldPromiseId, oldPromise);
    }

    const promise = new Promise((resolve) => {
      promiseRing[idx] = resolve;
    });
    const wrappedPromise = PromisePrototypeThen(
      promise,
      unwrapOpError(),
    );
    wrappedPromise[promiseIdSymbol] = promiseId;
    return wrappedPromise;
  }

  function __resolvePromise(promiseId, res) {
    // Check if out of ring bounds, fallback to map
    const outOfBounds = promiseId < nextPromiseId - RING_SIZE;
    if (outOfBounds) {
      const promise = MapPrototypeGet(promiseMap, promiseId);
      if (!promise) {
        throw "Missing promise in map @ " + promiseId;
      }
      MapPrototypeDelete(promiseMap, promiseId);
      promise(res);
    } else {
      // Otherwise take from ring
      const idx = promiseId % RING_SIZE;
      const promise = promiseRing[idx];
      if (!promise) {
        throw "Missing promise in ring @ " + promiseId;
      }
      promiseRing[idx] = NO_PROMISE;
      promise(res);
    }
  }

  function hasPromise(promiseId) {
    // Check if out of ring bounds, fallback to map
    const outOfBounds = promiseId < nextPromiseId - RING_SIZE;
    if (outOfBounds) {
      return MapPrototypeHas(promiseMap, promiseId);
    }
    // Otherwise check it in ring
    const idx = promiseId % RING_SIZE;
    return promiseRing[idx] != NO_PROMISE;
  }

  function unwrapOpError() {
    return (res) => {
      // .$err_class_name is a special key that should only exist on errors
      const className = res?.$err_class_name;
      if (!className) {
        return res;
      }

      const errorBuilder = errorMap[className];
      const err = errorBuilder ? errorBuilder(res.message) : new Error(
        `Unregistered error class: "${className}"\n  ${res.message}\n  Classes of errors returned from ops should be registered via Deno.core.registerErrorClass().`,
      );
      // Set .code if error was a known OS error, see error_codes.rs
      if (res.code) {
        err.code = res.code;
      }
      // Strip eventLoopTick() calls from stack trace
      ErrorCaptureStackTrace(err, eventLoopTick);
      throw err;
    };
  }

  function setUpAsyncStub(opName, originalOp) {
    let fn;
    // The body of this switch statement can be generated using the script above.
    switch (originalOp.length - 1) {
      /* BEGIN TEMPLATE setUpAsyncStub */
      /* DO NOT MODIFY: use rebuild_async_stubs.js to regenerate */
      case 0:
        fn = function async_op_0() {
          const id = nextPromiseId;
          try {
            const maybeResult = originalOp(id);
            if (maybeResult !== undefined) {
              return PromiseResolve(maybeResult);
            }
          } catch (err) {
            ErrorCaptureStackTrace(err, async_op_0);
            return PromiseReject(err);
          }
          if (isOpCallTracingEnabled) {
            submitOpCallTrace(id);
          }
          nextPromiseId = (id + 1) & 0xffffffff;
          return setPromise(id);
        };
        break;
      case 1:
        fn = function async_op_1(a) {
          const id = nextPromiseId;
          try {
            const maybeResult = originalOp(id, a);
            if (maybeResult !== undefined) {
              return PromiseResolve(maybeResult);
            }
          } catch (err) {
            ErrorCaptureStackTrace(err, async_op_1);
            return PromiseReject(err);
          }
          if (isOpCallTracingEnabled) {
            submitOpCallTrace(id);
          }
          nextPromiseId = (id + 1) & 0xffffffff;
          return setPromise(id);
        };
        break;
      case 2:
        fn = function async_op_2(a, b) {
          const id = nextPromiseId;
          try {
            const maybeResult = originalOp(id, a, b);
            if (maybeResult !== undefined) {
              return PromiseResolve(maybeResult);
            }
          } catch (err) {
            ErrorCaptureStackTrace(err, async_op_2);
            return PromiseReject(err);
          }
          if (isOpCallTracingEnabled) {
            submitOpCallTrace(id);
          }
          nextPromiseId = (id + 1) & 0xffffffff;
          return setPromise(id);
        };
        break;
      case 3:
        fn = function async_op_3(a, b, c) {
          const id = nextPromiseId;
          try {
            const maybeResult = originalOp(id, a, b, c);
            if (maybeResult !== undefined) {
              return PromiseResolve(maybeResult);
            }
          } catch (err) {
            ErrorCaptureStackTrace(err, async_op_3);
            return PromiseReject(err);
          }
          if (isOpCallTracingEnabled) {
            submitOpCallTrace(id);
          }
          nextPromiseId = (id + 1) & 0xffffffff;
          return setPromise(id);
        };
        break;
      case 4:
        fn = function async_op_4(a, b, c, d) {
          const id = nextPromiseId;
          try {
            const maybeResult = originalOp(id, a, b, c, d);
            if (maybeResult !== undefined) {
              return PromiseResolve(maybeResult);
            }
          } catch (err) {
            ErrorCaptureStackTrace(err, async_op_4);
            return PromiseReject(err);
          }
          if (isOpCallTracingEnabled) {
            submitOpCallTrace(id);
          }
          nextPromiseId = (id + 1) & 0xffffffff;
          return setPromise(id);
        };
        break;
      case 5:
        fn = function async_op_5(a, b, c, d, e) {
          const id = nextPromiseId;
          try {
            const maybeResult = originalOp(id, a, b, c, d, e);
            if (maybeResult !== undefined) {
              return PromiseResolve(maybeResult);
            }
          } catch (err) {
            ErrorCaptureStackTrace(err, async_op_5);
            return PromiseReject(err);
          }
          if (isOpCallTracingEnabled) {
            submitOpCallTrace(id);
          }
          nextPromiseId = (id + 1) & 0xffffffff;
          return setPromise(id);
        };
        break;
      case 6:
        fn = function async_op_6(a, b, c, d, e, f) {
          const id = nextPromiseId;
          try {
            const maybeResult = originalOp(id, a, b, c, d, e, f);
            if (maybeResult !== undefined) {
              return PromiseResolve(maybeResult);
            }
          } catch (err) {
            ErrorCaptureStackTrace(err, async_op_6);
            return PromiseReject(err);
          }
          if (isOpCallTracingEnabled) {
            submitOpCallTrace(id);
          }
          nextPromiseId = (id + 1) & 0xffffffff;
          return setPromise(id);
        };
        break;
      case 7:
        fn = function async_op_7(a, b, c, d, e, f, g) {
          const id = nextPromiseId;
          try {
            const maybeResult = originalOp(id, a, b, c, d, e, f, g);
            if (maybeResult !== undefined) {
              return PromiseResolve(maybeResult);
            }
          } catch (err) {
            ErrorCaptureStackTrace(err, async_op_7);
            return PromiseReject(err);
          }
          if (isOpCallTracingEnabled) {
            submitOpCallTrace(id);
          }
          nextPromiseId = (id + 1) & 0xffffffff;
          return setPromise(id);
        };
        break;
      case 8:
        fn = function async_op_8(a, b, c, d, e, f, g, h) {
          const id = nextPromiseId;
          try {
            const maybeResult = originalOp(id, a, b, c, d, e, f, g, h);
            if (maybeResult !== undefined) {
              return PromiseResolve(maybeResult);
            }
          } catch (err) {
            ErrorCaptureStackTrace(err, async_op_8);
            return PromiseReject(err);
          }
          if (isOpCallTracingEnabled) {
            submitOpCallTrace(id);
          }
          nextPromiseId = (id + 1) & 0xffffffff;
          return setPromise(id);
        };
        break;
      case 9:
        fn = function async_op_9(a, b, c, d, e, f, g, h, i) {
          const id = nextPromiseId;
          try {
            const maybeResult = originalOp(id, a, b, c, d, e, f, g, h, i);
            if (maybeResult !== undefined) {
              return PromiseResolve(maybeResult);
            }
          } catch (err) {
            ErrorCaptureStackTrace(err, async_op_9);
            return PromiseReject(err);
          }
          if (isOpCallTracingEnabled) {
            submitOpCallTrace(id);
          }
          nextPromiseId = (id + 1) & 0xffffffff;
          return setPromise(id);
        };
        break;
      /* END TEMPLATE */

      default:
        throw new Error(
          `Too many arguments for async op codegen (length of ${opName} was ${
            originalOp.length - 1
          })`,
        );
    }
    ObjectDefineProperty(fn, "name", {
      value: opName,
      configurable: false,
      writable: false,
    });
    return fn;
  }

  // Extra Deno.core.* exports
  const core = ObjectAssign(globalThis.Deno.core, {
    build,
    setBuildInfo,
    registerErrorBuilder,
    buildCustomError,
    registerErrorClass,
    setUpAsyncStub,
    hasPromise,
    promiseIdSymbol,
    __resolvePromise,
    __setOpCallTracingEnabled,
    __initializeCoreMethods,
  });

  ObjectAssign(globalThis.__bootstrap, { core });
  ObjectAssign(globalThis.Deno, { core });
})(globalThis);
