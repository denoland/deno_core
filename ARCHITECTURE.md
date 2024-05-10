# Architecture

## JsRuntime

The heart of Deno is the `JsRuntime`, which wraps a the v8 concepts of an
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

1. Resource management
2. Error and promise rejection handling
3. Event loop callbacks
4. Timers
5. Basic console support

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
