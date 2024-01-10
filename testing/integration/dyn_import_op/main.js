// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
import { barrierAwait, barrierCreate } from "checkin:async";

barrierCreate("barrier", 2);
(async () => {
  await import("./dynamic.js");
  await barrierAwait("barrier");
})();
