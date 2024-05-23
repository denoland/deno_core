# Architecture

## JsRuntime

The heart of Deno is the `JsRuntime`, which wraps the V8 concepts of an
`Isolate` and a `Context`. To create a runtime, an embedder instantiates a
`JsRuntime` using a set of `extension`s, and then poll the event loop until the
runtime has completed.

Each `extension` consists of a number of JavaScript- or TypeScript-language
ECMAScript modules, as well as a number of `op`s. This module code is loaded
when the runtime is created, allowing user code to access whatever system
services an embedder has decided to provide.

The `op`s provided by `extension`s allow for simple and fast JavaScript-to-Rust
function calls. An `op` may be sync or async (which makes it sync or async in
both the Rust and JavaScript world), and fallible or infallible. Most
fundamental V8 and Rust types are supported for `op` calls, and the system will
generally choose the most performant way to transport data regardless of type.

The runtime also provides persistent state in a number of forms: a
general-purpose `OpState` used by Rust code to stash state, `Resource`s that can
be read and written by both script and Rust code, and `cppgc` objects that can
be accessed by the Rust side and held on the JavaScript side (but not directly
accessed).

## Runtime Services

By default, v8 does not provide a fully-functional runtime. Many of the required
pieces to get a useful runtime are provided by the embedder. `deno_core`
provides a number of these useful services for you, including:

0. Extensions and `op`s
1. Resource management
2. Error and promise rejection handling
3. Event loop callbacks
4. Timers
5. Basic console support
6. Snapshot management

## Extensions and `op`s

Extensions provide additional functionality to Deno beyond the core runtime.
They can include `op`s for interacting with the runtime, ESM modules for
JavaScript code, and other features like configuration parameters and middleware
functions.

Extensions are defined using the `extension!` macro in `deno_core`. The options
available for defining an extension using the `extension!` macro allow for
fine-grained control over its behavior and dependencies.

When the runtime is initialized, it creates a V8 isolate and context, and then
instantiates the provided extension code and ops into the new context. The ops
are provided in a special, virtual module that can be imported from the
extension module code, allowing embedders to create fully-fledged APIs that are
backed by a combination of Rust, JavaScript or both types of code.

In general, APIs for user code are provided in one of two forms: global APIs,
available on `globalThis` (the equivalent of the `window` object in browsers),
and module-style APIs.

In the former style of API, the modules can add APIs to `globalThis` directly:

```ts
globalThis.Deno.myAPI = function myAPI() {
  op_do_something();
};
```

In the latter style of API, modules can be given friendly names in the
`extension` definition, and user code can import them directly:

```rust
extension!(
    ...
    esm = [ "brand:api_module": "api.js" ]
);
```

```ts
import { myAPI } from "brand:api_module";
myAPI();
```

## Resource management

Deno provides the equivalent of system streams in the form of `Resource`s. A
`Resource` is a Rust struct that implements a `Resource` trait. This trait
allows for explicit lifetime management: a unique integer value is associated
with the struct that can be used to explicitly close the resource when not
needed.

A `Resource` is instantiated inside of an `op`, and the integer handle is
returned to the calling code.

The `close` and `tryClose` operations are available as a `Deno.core` JS API, and
both take the integer ID of the resource.

A `Resource` may or may not implement stream read and write operations. These
are exposed via `read*` and `write*` APIs on `Deno.core`, and the APIs are
mapped to the `Resource`'s read and write trait methods.

## Error and promise rejection handling

Embedders make use of two callbacks for unhandled promise rejections and
uncaught errors: `setReportExceptionCallback`,
`setUnhandledPromiseRejectionHandler`.

When an ES Module throws an exception at the top level, or code in a callback or
promise continuation throws an exception that is not handled, `Deno.core`
detects this as an unhandled exception and routes it to the
`setReportExceptionCallback` or `setUnhandledPromiseRejectionHandler` callbacks.

`Deno.core` automatically handles errors and promise rejects that bubble out of
user code and into the top-level module and rejects the promise for the module
itself.

If an error ends up bubbling up to `reportUnhandledException` or
`reportUnhandledPromiseRejection`, that error is exposed through the runtime's
polling interface and runtime execution is halted.

Custom error classes may be registered via the `registerErrorClass` API,
allowing for Rust code to throw strongly-typed exceptions to user JS code. The
`custom_error` function in `deno_core` may be used to return these custom error
classes.

## Event loop callbacks

In async JavaScript, forward progress is made for module and promise resolution
through event loop callbacks. These callbacks may be async `op` resolution,
timer resolution, module loads, or internal callbacks from NAPI or FFI code.

Event loop callbacks generally happen through `eventLoopTick` for convenience,
though some other events are dispatched directly. The `eventLoopTick` method
will often appear in stack traces.

In `JsRuntime`, `do_js_event_loop_tick_realm` polls the various possible sources
of forward progress and routes them to the V8 engine. While `deno_core` may
allow for these async operations to complete at any time, the results are only
forwarded to the V8 engine during the event dispatch phase of the event loop,
preventing callbacks into the engine at unexpected times.

## Timers

`deno_core` provides a number of high-level timer support APIs for embedders to
build on. `queueUserTimer`, `refTimer` and `unrefTimer` are sufficient to
implement both web-compatible and node-compatible timers.

Timers are dispatched as part of the `eventLoopTick` infrastructure, however
this may change in the future.

## Basic console support

`deno_core` provides a basic implementation of the V8 console that integrates
with the V8 inspector. Embedders are encouraged to supply their own more
functional console implementation, however.

## Snapshot management

For embedders that wish to optimize `JsRuntime` startup, `JsRuntimeForSnapshot`
allows for a runtime to be created that will be serialized to a V8 snapshot.
This snapshot data may be passed to a `JsRuntime` as it boots, allowing it to
pre-initialize a V8 isolate and context from earlier initialization work.
