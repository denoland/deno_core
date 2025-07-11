// Copyright 2018-2025 the Deno authors. MIT license.
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
        PromisePrototypeCatch,
        RangeError,
        ReferenceError,
        SafeArrayIterator,
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
    // TODO(bartlomieju): in the future use `v8::Private` so it's not visible
    // to users. Currently missing bindings.
    const promiseIdSymbol = SymbolFor("Deno.core.internalPromiseId");

    let isLeakTracingEnabled = false;

    function __setLeakTracingEnabled(enabled) {
        isLeakTracingEnabled = enabled;
    }

    function __isLeakTracingEnabled() {
        return isLeakTracingEnabled;
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

    function buildCustomError(className, message, additionalProperties) {
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
            if (additionalProperties) {
                const keys = [];
                for (const property of new SafeArrayIterator(additionalProperties)) {
                    const key = property[0];
                    if (!(key in error)) {
                        keys.push(key);
                        error[key] = property[1];
                    }
                }
                Object.defineProperty(error, SymbolFor("errorAdditionalPropertyKeys"), {
                    value: keys,
                    writable: false,
                    enumerable: false,
                    configurable: false,
                });
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

    // Extra Deno.core.* exports
    const core = ObjectAssign(globalThis.Deno.core, {
        build,
        setBuildInfo,
        registerErrorBuilder,
        buildCustomError,
        registerErrorClass,
        promiseIdSymbol,
    });

    const infra = {
        __setLeakTracingEnabled,
        __isLeakTracingEnabled,
    };

    ObjectAssign(globalThis, { __infra: infra });
    ObjectAssign(globalThis.__bootstrap, { core });
    ObjectAssign(globalThis.Deno, { core });
})(globalThis);
