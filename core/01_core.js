// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
"use strict";

((window) => {
  const {
    ArrayPrototypeMap,
    ArrayPrototypePush,
    Error,
    ErrorCaptureStackTrace,
    FunctionPrototypeBind,
    ObjectAssign,
    ObjectFreeze,
    ObjectFromEntries,
    ObjectKeys,
    ObjectHasOwn,
    setQueueMicrotask,
    SafeMap,
    SafeWeakMap,
    Set,
    StringPrototypeSlice,
    Symbol,
    SymbolFor,
    TypedArrayPrototypeGetLength,
    TypedArrayPrototypeJoin,
    TypedArrayPrototypeSlice,
    TypedArrayPrototypeGetSymbolToStringTag,
    TypeError,
  } = window.__bootstrap.primordials;
  const {
    ops,
    hasPromise,
    promiseIdSymbol,
    registerErrorClass,
  } = window.Deno.core;
  const {
    __setLeakTracingEnabled,
    __isLeakTracingEnabled,
    __initializeCoreMethods,
    __resolvePromise,
  } = window.__infra;
  const {
    op_abort_wasm_streaming,
    op_current_user_call_site,
    op_decode,
    op_deserialize,
    op_destructure_error,
    op_dispatch_exception,
    op_encode,
    op_encode_binary_string,
    op_eval_context,
    op_event_loop_has_more_work,
    op_get_extras_binding_object,
    op_get_promise_details,
    op_get_proxy_details,
    op_has_tick_scheduled,
    op_lazy_load_esm,
    op_memory_usage,
    op_op_names,
    op_print,
    op_queue_microtask,
    op_ref_op,
    op_resources,
    op_run_microtasks,
    op_serialize,
    op_add_main_module_handler,
    op_set_handled_promise_rejection_handler,
    op_set_has_tick_scheduled,
    op_set_promise_hooks,
    op_set_wasm_streaming_callback,
    op_str_byte_length,
    op_timer_cancel,
    op_timer_queue,
    op_timer_queue_system,
    op_timer_queue_immediate,
    op_timer_ref,
    op_timer_unref,
    op_unref_op,
    op_cancel_handle,
    op_leak_tracing_enable,
    op_leak_tracing_submit,
    op_leak_tracing_get_all,
    op_leak_tracing_get,

    op_is_any_array_buffer,
    op_is_arguments_object,
    op_is_array_buffer,
    op_is_array_buffer_view,
    op_is_async_function,
    op_is_big_int_object,
    op_is_boolean_object,
    op_is_boxed_primitive,
    op_is_data_view,
    op_is_date,
    op_is_generator_function,
    op_is_generator_object,
    op_is_map,
    op_is_map_iterator,
    op_is_module_namespace_object,
    op_is_native_error,
    op_is_number_object,
    op_is_promise,
    op_is_proxy,
    op_is_reg_exp,
    op_is_set,
    op_is_set_iterator,
    op_is_shared_array_buffer,
    op_is_string_object,
    op_is_symbol_object,
    op_is_typed_array,
    op_is_weak_map,
    op_is_weak_set,
  } = ops;

  const {
    getContinuationPreservedEmbedderData,
    setContinuationPreservedEmbedderData,
  } = op_get_extras_binding_object();

  // core/infra collaborative code
  delete window.__infra;

  __initializeCoreMethods(
    eventLoopTick,
    submitLeakTrace,
  );

  function submitLeakTrace(id) {
    const error = new Error();
    ErrorCaptureStackTrace(error, submitLeakTrace);
    // "Error\n".length == 6
    op_leak_tracing_submit(0, id, StringPrototypeSlice(error.stack, 6));
  }

  function submitTimerTrace(id) {
    const error = new Error();
    ErrorCaptureStackTrace(error, submitTimerTrace);
    // We submit interval and timer traces as type "Timer"
    // "Error\n".length == 6
    op_leak_tracing_submit(2, id, StringPrototypeSlice(error.stack, 6));
  }

  let unhandledPromiseRejectionHandler = () => false;
  let timerDepth = 0;
  let timersRunning = false;
  const cancelledTimers = new Set();

  const macrotaskCallbacks = [];
  const nextTickCallbacks = [];

  function setMacrotaskCallback(cb) {
    ArrayPrototypePush(macrotaskCallbacks, cb);
  }

  function setNextTickCallback(cb) {
    ArrayPrototypePush(nextTickCallbacks, cb);
  }

  // This function has variable number of arguments. The last argument describes
  // if there's a "next tick" scheduled by the Node.js compat layer. Arguments
  // before last are alternating integers and any values that describe the
  // responses of async ops.
  function eventLoopTick() {
    // First respond to all pending ops.
    for (let i = 0; i < arguments.length - 3; i += 2) {
      const promiseId = arguments[i];
      const res = arguments[i + 1];
      __resolvePromise(promiseId, res);
    }
    // Drain nextTick queue if there's a tick scheduled.
    if (arguments[arguments.length - 1]) {
      for (let i = 0; i < nextTickCallbacks.length; i++) {
        nextTickCallbacks[i]();
      }
    } else {
      op_run_microtasks();
    }

    // Finally drain macrotask queue.
    for (let i = 0; i < macrotaskCallbacks.length; i++) {
      const cb = macrotaskCallbacks[i];
      while (true) {
        const res = cb();

        // If callback returned `undefined` then it has no work to do, we don't
        // need to perform microtask checkpoint.
        if (res === undefined) {
          break;
        }

        op_run_microtasks();
        // If callback returned `true` then it has no more work to do, stop
        // calling it then.
        if (res === true) {
          break;
        }
      }
    }

    const timers = arguments[arguments.length - 2];
    if (timers) {
      timersRunning = true;
      for (let i = 0; i < timers.length; i += 3) {
        timerDepth = timers[i];
        const id = timers[i + 1];
        if (cancelledTimers.has(id)) {
          continue;
        }
        try {
          const f = timers[i + 2];
          f.call(window);
        } catch (e) {
          reportExceptionCallback(e);
        }
        op_run_microtasks();
      }
      timersRunning = false;
      timerDepth = 0;
      cancelledTimers.clear();
    }

    // If we have any rejections for this tick, attempt to process them
    const rejections = arguments[arguments.length - 3];
    if (rejections) {
      for (let i = 0; i < rejections.length; i += 2) {
        const handled = unhandledPromiseRejectionHandler(
          rejections[i],
          rejections[i + 1],
        );
        if (!handled) {
          const err = rejections[i + 1];
          op_dispatch_exception(err, true);
        }
      }
    }
  }

  function refOp(promiseId) {
    if (!hasPromise(promiseId)) {
      return;
    }
    op_ref_op(promiseId);
  }

  function unrefOp(promiseId) {
    if (!hasPromise(promiseId)) {
      return;
    }
    op_unref_op(promiseId);
  }

  function refOpPromise(promise) {
    refOp(promise[promiseIdSymbol]);
  }

  function unrefOpPromise(promise) {
    unrefOp(promise[promiseIdSymbol]);
  }

  function resources() {
    return ObjectFromEntries(op_resources());
  }

  function metrics() {
    // TODO(mmastrac): we should replace this with a newer API
    return {
      opsDispatched: 0,
      opsDispatchedSync: 0,
      opsDispatchedAsync: 0,
      opsDispatchedAsyncUnref: 0,
      opsCompleted: 0,
      opsCompletedSync: 0,
      opsCompletedAsync: 0,
      opsCompletedAsyncUnref: 0,
      bytesSentControl: 0,
      bytesSentData: 0,
      bytesReceived: 0,
      ops: {},
    };
  }

  let reportExceptionCallback = (error) => {
    op_dispatch_exception(error, false);
  };

  // Used to report errors thrown from functions passed to `queueMicrotask()`.
  // The callback will be passed the thrown error. For example, you can use this
  // to dispatch an error event to the global scope.
  // In other words, set the implementation for
  // https://html.spec.whatwg.org/multipage/webappapis.html#report-the-exception
  function setReportExceptionCallback(cb) {
    if (cb === null || cb === undefined) {
      reportExceptionCallback = (error) => {
        op_dispatch_exception(error, false);
      };
    } else {
      if (typeof cb != "function") {
        throw new TypeError("expected a function");
      }
      reportExceptionCallback = cb;
    }
  }

  function queueMicrotask(cb) {
    if (typeof cb != "function") {
      throw new TypeError("expected a function");
    }
    return op_queue_microtask(() => {
      try {
        cb();
      } catch (error) {
        reportExceptionCallback(error);
      }
    });
  }

  // Some "extensions" rely on "BadResource", "Interrupted", "NotCapable"
  // errors in the JS code (eg. "deno_net") so they are provided in "Deno.core"
  // but later reexported on "Deno.errors"
  class BadResource extends Error {
    constructor(msg) {
      super(msg);
      this.name = "BadResource";
    }
  }
  const BadResourcePrototype = BadResource.prototype;

  class Interrupted extends Error {
    constructor(msg) {
      super(msg);
      this.name = "Interrupted";
    }
  }
  const InterruptedPrototype = Interrupted.prototype;

  class NotCapable extends Error {
    constructor(msg) {
      super(msg);
      this.name = "NotCapable";
    }
  }
  const NotCapablePrototype = NotCapable.prototype;

  registerErrorClass("BadResource", BadResource);
  registerErrorClass("Interrupted", Interrupted);
  registerErrorClass("NotCapable", NotCapable);

  const promiseHooks = [
    [], // init
    [], // before
    [], // after
    [], // resolve
  ];

  function setPromiseHooks(init, before, after, resolve) {
    const hooks = [init, before, after, resolve];
    for (let i = 0; i < hooks.length; i++) {
      const hook = hooks[i];
      // Skip if no callback was provided for this hook type.
      if (hook == null) {
        continue;
      }
      // Verify that the type of `hook` is a function.
      if (typeof hook !== "function") {
        throw new TypeError(`Expected function at position ${i}`);
      }
      // Add the hook to the list.
      ArrayPrototypePush(promiseHooks[i], hook);
    }

    const wrappedHooks = ArrayPrototypeMap(promiseHooks, (hooks) => {
      switch (hooks.length) {
        case 0:
          return undefined;
        case 1:
          return hooks[0];
        case 2:
          return create2xHookWrapper(hooks[0], hooks[1]);
        case 3:
          return create3xHookWrapper(hooks[0], hooks[1], hooks[2]);
        default:
          return createHookListWrapper(hooks);
      }

      // The following functions are used to create wrapper functions that call
      // all the hooks in a list of a certain length. The reason to use a
      // function that creates a wrapper is to minimize the number of objects
      // captured in the closure.
      function create2xHookWrapper(hook1, hook2) {
        return function (promise, parent) {
          hook1(promise, parent);
          hook2(promise, parent);
        };
      }
      function create3xHookWrapper(hook1, hook2, hook3) {
        return function (promise, parent) {
          hook1(promise, parent);
          hook2(promise, parent);
          hook3(promise, parent);
        };
      }
      function createHookListWrapper(hooks) {
        return function (promise, parent) {
          for (let i = 0; i < hooks.length; i++) {
            const hook = hooks[i];
            hook(promise, parent);
          }
        };
      }
    });

    op_set_promise_hooks(
      wrappedHooks[0],
      wrappedHooks[1],
      wrappedHooks[2],
      wrappedHooks[3],
    );
  }

  const {
    op_close: close,
    op_try_close: tryClose,
    op_read: read,
    op_read_all: readAll,
    op_write: write,
    op_write_all: writeAll,
    op_write_type_error: writeTypeError,
    op_read_sync: readSync,
    op_write_sync: writeSync,
    op_shutdown: shutdown,
    op_is_terminal: isTerminal,
  } = ops;

  const callSiteRetBuf = new Uint32Array(2);
  const callSiteRetBufU8 = new Uint8Array(callSiteRetBuf.buffer);

  function currentUserCallSite() {
    const fileName = op_current_user_call_site(callSiteRetBufU8);
    const lineNumber = callSiteRetBuf[0];
    const columnNumber = callSiteRetBuf[1];
    return { fileName, lineNumber, columnNumber };
  }

  const hostObjectBrand = SymbolFor("Deno.core.hostObject");

  // A helper function that will bind our own console implementation
  // with default implementation of Console from V8. This will cause
  // console messages to be piped to inspector console.
  //
  // We are using `Deno.core.callConsole` binding to preserve proper stack
  // frames in inspector console. This has to be done because V8 considers
  // the last JS stack frame as gospel for the inspector. In our case we
  // specifically want the latest user stack frame to be the one that matters
  // though.
  //
  // Inspired by:
  // https://github.com/nodejs/node/blob/1317252dfe8824fd9cfee125d2aaa94004db2f3b/lib/internal/util/inspector.js#L39-L61
  function wrapConsole(customConsole, consoleFromV8) {
    const callConsole = window.Deno.core.callConsole;

    const keys = ObjectKeys(consoleFromV8);
    for (let i = 0; i < keys.length; ++i) {
      const key = keys[i];
      if (ObjectHasOwn(customConsole, key)) {
        customConsole[key] = FunctionPrototypeBind(
          callConsole,
          customConsole,
          consoleFromV8[key],
          customConsole[key],
        );
      } else {
        // Add additional console APIs from the inspector
        customConsole[key] = consoleFromV8[key];
      }
    }
  }

  // Minimal console implementation, that uses `Deno.core.print` under the hood.
  // It's not fully fledged and is meant to make debugging slightly easier when working with
  // only `deno_core` crate.
  class CoreConsole {
    log = (...args) => {
      op_print(`${consoleStringify(...args)}\n`, false);
    };

    debug = (...args) => {
      op_print(`${consoleStringify(...args)}\n`, false);
    };

    warn = (...args) => {
      op_print(`${consoleStringify(...args)}\n`, false);
    };

    error = (...args) => {
      op_print(`${consoleStringify(...args)}\n`, false);
    };
  }

  const consoleStringify = (...args) => args.map(consoleStringifyArg).join(" ");

  const consoleStringifyArg = (arg) => {
    if (
      typeof arg === "string" || typeof arg === "boolean" ||
      typeof arg === "number" || arg === null || arg === undefined
    ) {
      return arg;
    }
    const tag = TypedArrayPrototypeGetSymbolToStringTag(arg);
    if (op_is_typed_array(arg)) {
      return `${tag}(${TypedArrayPrototypeGetLength(arg)}) [${
        TypedArrayPrototypeJoin(TypedArrayPrototypeSlice(arg, 0, 10), ", ")
      }]`;
    }
    if (tag !== undefined) {
      tag + " " + JSON.stringify(arg, undefined, 2);
    } else {
      return JSON.stringify(arg, undefined, 2);
    }
  };

  const v8Console = globalThis.console;
  const coreConsole = new CoreConsole();
  globalThis.console = coreConsole;
  wrapConsole(coreConsole, v8Console);

  function propWritable(value) {
    return {
      value,
      writable: true,
      enumerable: true,
      configurable: true,
    };
  }

  function propNonEnumerable(value) {
    return {
      value,
      writable: true,
      enumerable: false,
      configurable: true,
    };
  }

  function propReadOnly(value) {
    return {
      value,
      enumerable: true,
      writable: false,
      configurable: true,
    };
  }

  function propGetterOnly(getter) {
    return {
      get: getter,
      set() {},
      enumerable: true,
      configurable: true,
    };
  }

  function propWritableLazyLoaded(getter, loadFn) {
    let valueIsSet = false;
    let value;

    return {
      get() {
        const loadedValue = loadFn();
        if (valueIsSet) {
          return value;
        } else {
          return getter(loadedValue);
        }
      },
      set(v) {
        loadFn();
        valueIsSet = true;
        value = v;
      },
      enumerable: true,
      configurable: true,
    };
  }

  function propNonEnumerableLazyLoaded(getter, loadFn) {
    let valueIsSet = false;
    let value;

    return {
      get() {
        const loadedValue = loadFn();
        if (valueIsSet) {
          return value;
        } else {
          return getter(loadedValue);
        }
      },
      set(v) {
        loadFn();
        valueIsSet = true;
        value = v;
      },
      enumerable: false,
      configurable: true,
    };
  }

  function createLazyLoader(specifier) {
    let value;

    return function lazyLoad() {
      if (!value) {
        value = op_lazy_load_esm(specifier);
      }
      return value;
    };
  }

  const getAsyncContext = getContinuationPreservedEmbedderData;
  const setAsyncContext = setContinuationPreservedEmbedderData;

  let asyncVariableCounter = 0;
  class AsyncVariable {
    #id = asyncVariableCounter++;
    #data = new SafeWeakMap();

    enter(value) {
      const previousContextMapping = getAsyncContext();
      const entry = { id: this.#id };
      const asyncContextMapping = {
        __proto__: null,
        ...previousContextMapping,
        [this.#id]: entry,
      };
      this.#data.set(entry, value);
      setAsyncContext(asyncContextMapping);
      return previousContextMapping;
    }

    get() {
      const current = getAsyncContext();
      const entry = current?.[this.#id];
      if (entry) {
        return this.#data.get(entry);
      }
      return undefined;
    }
  }

  // Extra Deno.core.* exports
  const core = ObjectAssign(globalThis.Deno.core, {
    internalRidSymbol: Symbol("Deno.internal.rid"),
    resources,
    metrics,
    eventLoopTick,
    BadResource,
    BadResourcePrototype,
    Interrupted,
    InterruptedPrototype,
    NotCapable,
    NotCapablePrototype,
    refOpPromise,
    unrefOpPromise,
    setReportExceptionCallback,
    setPromiseHooks,
    consoleStringify,
    close,
    tryClose,
    read,
    readAll,
    write,
    writeAll,
    writeTypeError,
    readSync,
    writeSync,
    shutdown,
    isTerminal,
    print: (msg, isErr) => op_print(msg, isErr),
    setLeakTracingEnabled: (enabled) => {
      __setLeakTracingEnabled(enabled);
      op_leak_tracing_enable(enabled);
    },
    isLeakTracingEnabled: () => __isLeakTracingEnabled(),
    getAllLeakTraces: () => {
      const traces = op_leak_tracing_get_all();
      return new SafeMap(traces);
    },
    getLeakTraceForPromise: (promise) =>
      op_leak_tracing_get(0, promise[promiseIdSymbol]),
    setMacrotaskCallback,
    setNextTickCallback,
    runMicrotasks: () => op_run_microtasks(),
    hasTickScheduled: () => op_has_tick_scheduled(),
    setHasTickScheduled: (bool) => op_set_has_tick_scheduled(bool),
    evalContext: (
      source,
      specifier,
      hostDefinedOptions,
    ) => {
      const [result, error] = op_eval_context(
        source,
        specifier,
        hostDefinedOptions,
      );
      if (error) {
        const { 0: thrown, 1: isNativeError, 2: isCompileError } = error;
        return [
          result,
          {
            thrown,
            isNativeError,
            isCompileError,
          },
        ];
      }
      return [result, null];
    },
    hostObjectBrand,
    encode: (text) => op_encode(text),
    encodeBinaryString: (buffer) => op_encode_binary_string(buffer),
    decode: (buffer) => op_decode(buffer),
    serialize: (
      value,
      options,
      errorCallback,
    ) => {
      return op_serialize(
        value,
        options?.hostObjects,
        options?.transferredArrayBuffers,
        options?.forStorage ?? false,
        errorCallback,
      );
    },
    deserialize: (buffer, options) => {
      return op_deserialize(
        buffer,
        options?.hostObjects,
        options?.transferredArrayBuffers,
        options?.forStorage ?? false,
      );
    },
    getPromiseDetails: (promise) => op_get_promise_details(promise),
    getProxyDetails: (proxy) => op_get_proxy_details(proxy),
    isAnyArrayBuffer: (value) => op_is_any_array_buffer(value),
    isArgumentsObject: (value) => op_is_arguments_object(value),
    isArrayBuffer: (value) => op_is_array_buffer(value),
    isArrayBufferView: (value) => op_is_array_buffer_view(value),
    isAsyncFunction: (value) => op_is_async_function(value),
    isBigIntObject: (value) => op_is_big_int_object(value),
    isBooleanObject: (value) => op_is_boolean_object(value),
    isBoxedPrimitive: (value) => op_is_boxed_primitive(value),
    isDataView: (value) => op_is_data_view(value),
    isDate: (value) => op_is_date(value),
    isGeneratorFunction: (value) => op_is_generator_function(value),
    isGeneratorObject: (value) => op_is_generator_object(value),
    isMap: (value) => op_is_map(value),
    isMapIterator: (value) => op_is_map_iterator(value),
    isModuleNamespaceObject: (value) => op_is_module_namespace_object(value),
    isNativeError: (value) => op_is_native_error(value),
    isNumberObject: (value) => op_is_number_object(value),
    isPromise: (value) => op_is_promise(value),
    isProxy: (value) => op_is_proxy(value),
    isRegExp: (value) => op_is_reg_exp(value),
    isSet: (value) => op_is_set(value),
    isSetIterator: (value) => op_is_set_iterator(value),
    isSharedArrayBuffer: (value) => op_is_shared_array_buffer(value),
    isStringObject: (value) => op_is_string_object(value),
    isSymbolObject: (value) => op_is_symbol_object(value),
    isTypedArray: (value) => op_is_typed_array(value),
    isWeakMap: (value) => op_is_weak_map(value),
    isWeakSet: (value) => op_is_weak_set(value),
    memoryUsage: () => op_memory_usage(),
    setWasmStreamingCallback: (fn) => op_set_wasm_streaming_callback(fn),
    abortWasmStreaming: (
      rid,
      error,
    ) => op_abort_wasm_streaming(rid, error),
    destructureError: (error) => op_destructure_error(error),
    opNames: () => op_op_names(),
    eventLoopHasMoreWork: () => op_event_loop_has_more_work(),
    byteLength: (str) => op_str_byte_length(str),
    addMainModuleHandler: (handler) => op_add_main_module_handler(handler),
    setHandledPromiseRejectionHandler: (handler) =>
      op_set_handled_promise_rejection_handler(handler),
    setUnhandledPromiseRejectionHandler: (handler) =>
      unhandledPromiseRejectionHandler = handler,
    reportUnhandledException: (e) => op_dispatch_exception(e, false),
    reportUnhandledPromiseRejection: (e) => op_dispatch_exception(e, true),
    queueUserTimer: (depth, repeat, timeout, task) => {
      const id = op_timer_queue(depth, repeat, timeout, task);
      if (__isLeakTracingEnabled()) {
        submitTimerTrace(id);
      }
      return id;
    },
    // TODO(mmastrac): Hook up associatedOp to tracing
    queueSystemTimer: (_associatedOp, repeat, timeout, task) =>
      op_timer_queue_system(repeat, timeout, task),
    queueImmediate: (task) => op_timer_queue_immediate(task),
    cancelTimer: (id) => {
      if (timersRunning) {
        cancelledTimers.add(id);
      }
      op_timer_cancel(id);
    },
    refTimer: (id) => op_timer_ref(id),
    unrefTimer: (id) => op_timer_unref(id),
    getTimerDepth: () => timerDepth,
    currentUserCallSite,
    wrapConsole,
    v8Console,
    propReadOnly,
    propWritable,
    propNonEnumerable,
    propGetterOnly,
    propWritableLazyLoaded,
    propNonEnumerableLazyLoaded,
    createLazyLoader,
    createCancelHandle: () => op_cancel_handle(),
    getAsyncContext,
    setAsyncContext,
    AsyncVariable,
  });

  const internals = {};
  ObjectAssign(globalThis.__bootstrap, { core, internals });
  ObjectAssign(globalThis.Deno, { core });
  ObjectFreeze(globalThis.__bootstrap.core);

  // Direct bindings on `globalThis`
  ObjectAssign(globalThis, { queueMicrotask });
  setQueueMicrotask(queueMicrotask);
})(globalThis);
