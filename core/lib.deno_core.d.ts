// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.

// deno-lint-ignore-file no-explicit-any

/// <reference no-default-lib="true" />
/// <reference lib="esnext" />

declare namespace Deno {
  namespace core {
    /** Returns a proxy that generates the fast versions of sync and async ops. */
    function ensureFastOps(): any;

    /** Call an op in Rust, and asynchronously receive the result. */
    function opAsync(
      opName: string,
      ...args: any[]
    ): Promise<any>;

    /** Mark following promise as "ref", ie. event loop won't exit
     * until all "ref" promises are resolved. All async ops are "ref" by default. */
    function refOp(promiseId: number): void;

    /** Mark following promise as "unref", ie. event loop will exit
     * if there are only "unref" promises left. */
    function unrefOp(promiseId: number): void;

    /** Mark following promise as "ref", ie. event loop won't exit
     * until all "ref" promises are resolved. All async ops are "ref" by default. */
    function refOpPromise<T>(promise: Promise<T>): void;

    /** Mark following promise as "unref", ie. event loop will exit
     * if there are only "unref" promises left. */
    function unrefOpPromise<T>(promise: Promise<T>): void;

    /**
     * List of all registered ops, in the form of a map that maps op
     * name to function.
     */
    const ops: Record<string, (...args: unknown[]) => any>;

    /**
     * List of all registered async ops, in the form of a map that maps op
     * name to function.
     */
    const asyncOps: Record<string, (...args: unknown[]) => any>;

    /**
     * Retrieve a list of all open resources, in the form of a map that maps
     * resource id to the resource name.
     */
    function resources(): Record<string, string>;

    /**
     * Close the resource with the specified op id. Throws `BadResource` error
     * if resource doesn't exist in resource table.
     */
    function close(rid: number): void;

    /**
     * Try close the resource with the specified op id; if resource with given
     * id doesn't exist do nothing.
     */
    function tryClose(rid: number): void;

    /**
     * Read from a (stream) resource that implements read()
     */
    function read(rid: number, buf: Uint8Array): Promise<number>;

    /**
     * Write to a (stream) resource that implements write()
     */
    function write(rid: number, buf: Uint8Array): Promise<number>;

    /**
     * Write to a (stream) resource that implements write()
     */
    function writeAll(rid: number, buf: Uint8Array): Promise<void>;

    /**
     * Synchronously read from a (stream) resource that implements readSync().
     */
    function readSync(rid: number, buf: Uint8Array): number;

    /**
     * Synchronously write to a (stream) resource that implements writeSync().
     */
    function writeSync(rid: number, buf: Uint8Array): number;

    /**
     * Print a message to stdout or stderr
     */
    function print(message: string, is_err?: boolean): void;

    /**
     * Shutdown a resource
     */
    function shutdown(rid: number): Promise<void>;

    /** Encode a string to its Uint8Array representation. */
    function encode(input: string): Uint8Array;

    /**
     * Set a callback that will be called when the WebAssembly streaming APIs
     * (`WebAssembly.compileStreaming` and `WebAssembly.instantiateStreaming`)
     * are called in order to feed the source's bytes to the wasm compiler.
     * The callback is called with the source argument passed to the streaming
     * APIs and an rid to use with the wasm streaming ops.
     *
     * The callback should eventually invoke the following ops:
     *   - `op_wasm_streaming_feed`. Feeds bytes from the wasm resource to the
     *     compiler. Takes the rid and a `Uint8Array`.
     *   - `op_wasm_streaming_abort`. Aborts the wasm compilation. Takes the rid
     *     and an exception. Invalidates the resource.
     *   - `op_wasm_streaming_set_url`. Sets a source URL for the wasm module.
     *     Takes the rid and a string.
     *   - To indicate the end of the resource, use `Deno.core.close()` with the
     *     rid.
     */
    function setWasmStreamingCallback(
      cb: (source: any, rid: number) => void,
    ): void;

    /**
     * Set a callback that will be called after resolving ops and before resolving
     * macrotasks.
     */
    function setNextTickCallback(
      cb: () => void,
    ): void;

    /** Check if there's a scheduled "next tick". */
    function hasNextTickScheduled(): boolean;

    /** Set a value telling the runtime if there are "next ticks" scheduled */
    function setHasNextTickScheduled(value: boolean): void;

    /**
     * Set a callback that will be called after resolving ops and "next ticks".
     */
    function setMacrotaskCallback(
      cb: () => boolean,
    ): void;

    /**
     * Sets the unhandled promise rejection handler. The handler returns 'true' if the
     * rejection has been handled. If the handler returns 'false', the promise is considered
     * unhandled, and the runtime then raises an uncatchable error and halts.
     */
    function setUnhandledPromiseRejectionHandler(
      cb: PromiseRejectCallback,
    ): PromiseRejectCallback;

    export type PromiseRejectCallback = (
      promise: Promise<unknown>,
      reason: any,
    ) => boolean;

    /**
     * Report an exception that was not handled by any runtime handler, and escaped to the
     * top level. This terminates the runtime.
     */
    function reportUnhandledException(e: Error): void;

    /**
     * Report an unhandled promise rejection that was not handled by any runtime handler, and
     * escaped to the top level. This terminates the runtime.
     */
    function reportUnhandledPromiseRejection(e: Error): void;

    /**
     * Set a callback that will be called when an exception isn't caught
     * by any try/catch handlers. Currently only invoked when the callback
     * to setPromiseRejectCallback() throws an exception but that is expected
     * to change in the future. Returns the old handler or undefined.
     */
    function setUncaughtExceptionCallback(
      cb: UncaughtExceptionCallback,
    ): undefined | UncaughtExceptionCallback;

    export type UncaughtExceptionCallback = (err: any) => void;

    function serialize(
      value: any,
      options?: any,
      errorCallback?,
    ): Uint8Array;

    function deserialize(buffer: Uint8Array, options?: any): any;

    /**
     * Enables collection of stack traces of all async ops. This allows for
     * debugging of where a given async op was started. Deno CLI uses this for
     * improving error message in op sanitizer errors for `deno test`.
     *
     * **NOTE:** enabling tracing has a significant negative performance impact.
     * To get high level metrics on async ops with no added performance cost,
     * use `Deno.core.metrics()`.
     */
    function enableOpCallTracing(): void;

    export interface OpCallTrace {
      opName: string;
      stack: string;
    }

    /**
     * A map containing traces for all ongoing async ops. The key is the op id.
     * Tracing only occurs when `Deno.core.enableOpCallTracing()` was previously
     * enabled.
     */
    const opCallTraces: Map<number, OpCallTrace>;

    /**
     * Adds a callback for the given Promise event. If this function is called
     * multiple times, the callbacks are called in the order they were added.
     * - `init_hook` is called when a new promise is created. When a new promise
     *   is created as part of the chain in the case of `Promise.then` or in the
     *   intermediate promises created by `Promise.{race, all}`/`AsyncFunctionAwait`,
     *   we pass the parent promise otherwise we pass undefined.
     * - `before_hook` is called at the beginning of the promise reaction.
     * - `after_hook` is called at the end of the promise reaction.
     * - `resolve_hook` is called at the beginning of resolve or reject function.
     */
    function setPromiseHooks(
      init_hook?: (
        promise: Promise<unknown>,
        parentPromise?: Promise<unknown>,
      ) => void,
      before_hook?: (promise: Promise<unknown>) => void,
      after_hook?: (promise: Promise<unknown>) => void,
      resolve_hook?: (promise: Promise<unknown>) => void,
    ): void;

    const build: {
      target: string;
      arch: string;
      os: string;
      vendor: string;
      env: string | undefined;
    };
  }
}
