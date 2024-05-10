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
