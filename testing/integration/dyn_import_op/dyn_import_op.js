// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
import "./main.js";
import { barrierAwait } from "checkin:async";
await barrierAwait("barrier");
console.log("done");
