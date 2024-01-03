// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
import * as testing from "checkin:testing";
import * as console from "checkin:console";
import * as timers from "checkin:timers";

testing;

globalThis.console = console.console;
globalThis.setTimeout = timers.setTimeout;
globalThis.setInterval = timers.setInterval;
globalThis.clearTimeout = timers.clearTimeout;
globalThis.clearInterval = timers.clearInterval;
Reflect.defineProperty(globalThis, "onunhandledrejection", {
  set: (cb) => {
    if (cb) {
      Deno.core.setUnhandledPromiseRejectionHandler((promise, reason) => {
        let defaultPrevented = false;
        cb({
          promise,
          reason,
          preventDefault: () => defaultPrevented = true,
        });
        return defaultPrevented;
      });
    } else {
      Deno.core.setUnhandledPromiseRejectionHandler(null);
    }
  },
});
Reflect.defineProperty(globalThis, "onrejectionhandled", {
  set: (cb) => {
    if (cb) {
      Deno.core.setHandledPromiseRejectionHandler((promise, reason) => {
        cb({
          promise,
          reason,
        });
      });
    } else {
      Deno.core.setHandledPromiseRejectionHandler(null);
    }
  },
});
Deno.unrefTimer = timers.unrefTimer;
Deno.refTimer = timers.refTimer;
