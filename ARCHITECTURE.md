# Architecture

## JsRuntime

The heart of Deno is the `JsRuntime`, which wraps a the v8 concepts of an `Isolate` and a `Context`. To create
a runtime, an embedder instantiates a `JsRuntime` using a set of `extension`s, and then poll the event loop until
the runtime has completed.

Each `extension` consists of a number of JavaScript- or TypeScript-language ECMAScript modules, as well as a number
of `op`s. This module code is loaded when the runtime is created, allowing user code to access whatever system services
an embedder has decided to provide.

The `op`s provided by `extension`s allow for simple and fast JavaScript-to-Rust function calls. An `op` may be
sync or async (which makes it sync or async in both the Rust and JavaScript world), and fallible or infallible.
Most fundamental V8 and Rust types are supported for `op` calls, and the system will generally choose the most
performant way to transport data regardless of type.

