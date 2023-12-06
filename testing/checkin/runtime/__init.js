// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
import * as testing from "checkin:testing";
import * as console from "checkin:console";

testing;

globalThis.console = console.console;
