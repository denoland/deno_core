import { asyncNeverResolves } from "checkin:async";

// make a promise that never resolves to keep the event loop
// alive without scheduling it to be woken up
// for polling
const prom = asyncNeverResolves();

// import a module, with the key being that
// this module promise doesn't resolve until a later
// tick of the event loop
await import("./dynamic.js");
console.log("module imported");

// unref to let the event loop exit
Deno.core.unrefOpPromise(prom);
