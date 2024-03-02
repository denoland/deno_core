// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
macro_rules! v8_static_strings {
  ($($ident:ident = $str:literal),* $(,)?) => {
    $(
      pub static $ident: $crate::FastStaticString = $crate::ascii_str!($str);
    )*
  };
}

v8_static_strings!(
  DENO = "Deno",
  CORE = "core",
  OPS = "ops",
  URL = "url",
  MAIN = "main",
  RESOLVE = "resolve",
  MESSAGE = "message",
  CODE = "code",
  ERR_MODULE_NOT_FOUND = "ERR_MODULE_NOT_FOUND",
  EVENT_LOOP_TICK = "eventLoopTick",
  BUILD_CUSTOM_ERROR = "buildCustomError",
  CONSOLE = "console",
  CALL_CONSOLE = "callConsole",
  FILENAME = "filename",
  DIRNAME = "dirname",
  SET_UP_ASYNC_STUB = "setUpAsyncStub",
  WEBASSEMBLY = "WebAssembly",
  INSTANTIATE = "instantiate",
  WASM_INSTANTIATE = "wasmInstantiate",
);
