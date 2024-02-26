// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.

/**
 * Call a callback function after a delay.
 */
export function setTimeout(callback, delay = 0) {
  return Deno.core.queueUserTimer(
    Deno.core.getTimerDepth() + 1,
    false,
    delay,
    callback,
  );
}

/**
 * Call a callback function after a delay.
 */
export function setInterval(callback, delay = 0) {
  return Deno.core.queueUserTimer(
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

/**
 * Mark a timer as not blocking event loop exit.
 */
export function unrefTimer(id) {
  Deno.core.unrefTimer(id);
}

/**
 * Mark a timer as blocking event loop exit.
 */
export function refTimer(id) {
  Deno.core.refTimer(id);
}
