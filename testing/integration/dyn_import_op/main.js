import { barrierAwait, barrierCreate } from "checkin:async";

barrierCreate("barrier", 2);
(async () => {
  await import("./dynamic.js");
  await barrierAwait("barrier");
})();
