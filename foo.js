function setInterval(callback, delay = 0) {
  return Deno.core.queueUserTimer(
    Deno.core.getTimerDepth() + 1,
    true,
    delay,
    callback,
  );
}

console.log("starting interval 1s...");

setInterval(() => {
  console.log("ping...");
}, 1000);

console.log("Interval started, doing nothing...");
