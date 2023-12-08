// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.

/**
 * Call a callback function after a delay.
 */
export function setTimeout(callback, delay) {
  return Deno.core.queueTimer(
    Deno.core.getTimerDepth() + 1,
    false,
    delay,
    callback,
  );
}

/**
 * Call a callback function after a delay.
 */
export function setInterval(callback, delay) {
  return Deno.core.queueTimer(
    Deno.core.getTimerDepth() + 1,
    true,
    delay,
    callback,
  );
}

/**
 * Clear a timeout or interval.
 */
export function clearTimeout(id) {
  Deno.core.cancelTimer(id);
}

/**
 * Clear a timeout or interval.
 */
export function clearInterval(id) {
  Deno.core.cancelTimer(id);
}
