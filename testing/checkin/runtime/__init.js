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
Deno.unrefTimer = timers.unrefTimer;
Deno.refTimer = timers.refTimer;
