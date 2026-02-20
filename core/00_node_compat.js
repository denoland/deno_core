// Copyright 2018-2025 the Deno authors. MIT license.
// Ported from Node.js:
//   - lib/internal/fixed_queue.js
//   - lib/internal/process/task_queues.js
//   - lib/internal/timers.js
//   - lib/internal/process/promises.js
"use strict";

((window) => {
  const {
    Array,
    ArrayPrototypeFill,
    MathMax,
    MathTrunc,
    ObjectAssign,
    SafeMap,
    SafeWeakMap,
    Symbol,
  } = window.__bootstrap.primordials;

  // --------------------------------------------------------------------------
  // FixedQueue (from lib/internal/fixed_queue.js)
  // --------------------------------------------------------------------------

  // Currently optimal queue size, tested on V8 6.0 - 6.6. Must be power of two.
  const kSize = 2048;
  const kMask = kSize - 1;

  class FixedCircularBuffer {
    constructor() {
      this.bottom = 0;
      this.top = 0;
      this.list = ArrayPrototypeFill(new Array(kSize), undefined);
      this.next = null;
    }

    isEmpty() {
      return this.top === this.bottom;
    }

    isFull() {
      return ((this.top + 1) & kMask) === this.bottom;
    }

    push(data) {
      this.list[this.top] = data;
      this.top = (this.top + 1) & kMask;
    }

    shift() {
      const nextItem = this.list[this.bottom];
      if (nextItem === undefined)
        return null;
      this.list[this.bottom] = undefined;
      this.bottom = (this.bottom + 1) & kMask;
      return nextItem;
    }
  }

  class FixedQueue {
    constructor() {
      this.head = this.tail = new FixedCircularBuffer();
    }

    isEmpty() {
      return this.head.isEmpty();
    }

    push(data) {
      if (this.head.isFull()) {
        // Head is full: Creates a new queue, sets the old queue's `.next`
        // to it, and sets it as the new main queue.
        this.head = this.head.next = new FixedCircularBuffer();
      }
      this.head.push(data);
    }

    shift() {
      const tail = this.tail;
      const next = tail.shift();
      if (tail.isEmpty() && tail.next !== null) {
        // If there is another queue, it forms the new tail.
        this.tail = tail.next;
        tail.next = null;
      }
      return next;
    }
  }

  // --------------------------------------------------------------------------
  // Task Queue (from lib/internal/process/task_queues.js)
  // --------------------------------------------------------------------------

  // TODO: These would come from internalBinding('task_queue') in Node.
  // For now, we use Deno's ops as the backing mechanism.
  // const { tickInfo, runMicrotasks, setTickCallback, enqueueMicrotask }
  //   = internalBinding('task_queue');

  const kHasTickScheduled = 0;
  // Backing storage for tick state (mirrors Node's tickInfo TypedArray).
  const tickInfo = new Int32Array(2);

  function hasTickScheduled() {
    return tickInfo[kHasTickScheduled] === 1;
  }

  function setHasTickScheduled(value) {
    tickInfo[kHasTickScheduled] = value ? 1 : 0;
  }

  const tickQueue = new FixedQueue();

  // Should be in sync with RunNextTicksNative in node_task_queue.cc
  function runNextTicks() {
    if (!hasTickScheduled() && !hasRejectionToWarn()) {
      // TODO: call into V8 microtask queue via ops
      // runMicrotasks();
    }
    if (!hasTickScheduled() && !hasRejectionToWarn())
      return;

    processTicksAndRejections();
  }

  function processTicksAndRejections() {
    let tock;
    do {
      while ((tock = tickQueue.shift()) !== null) {
        // TODO: AsyncContextFrame support
        // const priorContextFrame =
        //   AsyncContextFrame.exchange(tock[async_context_frame]);

        // TODO: async_hooks emitBefore/emitAfter/emitDestroy
        // const asyncId = tock[async_id_symbol];
        // emitBefore(asyncId, tock[trigger_async_id_symbol], tock);

        try {
          const callback = tock.callback;
          if (tock.args === undefined) {
            callback();
          } else {
            const args = tock.args;
            switch (args.length) {
              case 1: callback(args[0]); break;
              case 2: callback(args[0], args[1]); break;
              case 3: callback(args[0], args[1], args[2]); break;
              case 4: callback(args[0], args[1], args[2], args[3]); break;
              default: callback(...args);
            }
          }
        } finally {
          // TODO: emitDestroy(asyncId);
        }

        // TODO: emitAfter(asyncId);
        // TODO: AsyncContextFrame.set(priorContextFrame);
      }
      // TODO: runMicrotasks();
    } while (!tickQueue.isEmpty() || processPromiseRejections());
    setHasTickScheduled(false);
    setHasRejectionToWarn(false);
  }

  // `nextTick()` will not enqueue any callback when the process is about to
  // exit since the callback would not have a chance to be executed.
  function nextTick(callback) {
    if (typeof callback !== "function") {
      throw new TypeError("callback must be a function");
    }

    // TODO: check process._exiting

    let args;
    switch (arguments.length) {
      case 1: break;
      case 2: args = [arguments[1]]; break;
      case 3: args = [arguments[1], arguments[2]]; break;
      case 4: args = [arguments[1], arguments[2], arguments[3]]; break;
      default:
        args = new Array(arguments.length - 1);
        for (let i = 1; i < arguments.length; i++)
          args[i - 1] = arguments[i];
    }

    if (tickQueue.isEmpty())
      setHasTickScheduled(true);

    // TODO: async_hooks newAsyncId/getDefaultTriggerAsyncId/emitInit
    // const asyncId = newAsyncId();
    // const triggerAsyncId = getDefaultTriggerAsyncId();
    const tickObject = {
      // [async_id_symbol]: asyncId,
      // [trigger_async_id_symbol]: triggerAsyncId,
      // [async_context_frame]: AsyncContextFrame.current(),
      callback,
      args,
    };
    // if (initHooksExist())
    //   emitInit(asyncId, 'TickObject', triggerAsyncId, tickObject);
    tickQueue.push(tickObject);
  }

  function setupTaskQueue() {
    // Sets the per-isolate promise rejection callback
    // TODO: listenForRejections();
    // Sets the callback to be run in every tick.
    // TODO: setTickCallback(processTicksAndRejections);
    return {
      nextTick,
      runNextTicks,
    };
  }

  // --------------------------------------------------------------------------
  // Timers (from lib/internal/timers.js)
  // --------------------------------------------------------------------------

  const TIMEOUT_MAX = 2 ** 31 - 1;

  // *Must* match Environment::ImmediateInfo::Fields in src/env.h.
  const kCount = 0;
  const kRefCount = 1;
  const kHasOutstanding = 2;

  // TODO: These would come from internalBinding('timers') in Node.
  // const { immediateInfo, timeoutInfo } = internalBinding('timers');
  const immediateInfo = new Int32Array(3);
  const timeoutInfo = new Int32Array(1);
  timeoutInfo[0] = 0;

  const kRefed = Symbol("refed");

  // A linked list for storing `setImmediate()` requests
  class ImmediateList {
    constructor() {
      this.head = null;
      this.tail = null;
    }

    append(item) {
      if (this.tail !== null) {
        this.tail._idleNext = item;
        item._idlePrev = this.tail;
      } else {
        this.head = item;
      }
      this.tail = item;
    }

    remove(item) {
      if (item._idleNext) {
        item._idleNext._idlePrev = item._idlePrev;
      }

      if (item._idlePrev) {
        item._idlePrev._idleNext = item._idleNext;
      }

      if (item === this.head)
        this.head = item._idleNext;
      if (item === this.tail)
        this.tail = item._idlePrev;

      item._idleNext = null;
      item._idlePrev = null;
    }
  }

  // Create a single linked list instance only once at startup
  const immediateQueue = new ImmediateList();

  // TODO: PriorityQueue and timerListQueue for full timer support
  // const timerListQueue = new PriorityQueue(compareTimersLists, setPosition);
  // const timerListMap = { __proto__: null };
  let nextExpiry = Infinity;

  function setupTimers() {
    // TODO: Wire up to the native timer bindings
    // This is called during bootstrap to set up the timer callbacks
    // In Node.js, this calls getTimerCallbacks(runNextTicks) which returns
    // { processImmediate, processTimers }
    return getTimerCallbacks(runNextTicks);
  }

  function getTimerCallbacks(runNextTicks_) {
    // If an uncaught exception was thrown during execution of immediateQueue,
    // this queue will store all remaining Immediates that need to run upon
    // resolution of all error handling (if process is still alive).
    const outstandingQueue = new ImmediateList();

    function processImmediate() {
      const queue = outstandingQueue.head !== null ?
        outstandingQueue : immediateQueue;
      let immediate = queue.head;

      // Clear the linked list early in case new `setImmediate()`
      // calls occur while immediate callbacks are executed
      if (queue !== outstandingQueue) {
        queue.head = queue.tail = null;
        immediateInfo[kHasOutstanding] = 1;
      }

      let prevImmediate;
      let ranAtLeastOneImmediate = false;
      while (immediate !== null) {
        if (ranAtLeastOneImmediate)
          runNextTicks_();
        else
          ranAtLeastOneImmediate = true;

        // It's possible for this current Immediate to be cleared while
        // executing the next tick queue above, which means we need to use
        // the previous Immediate's _idleNext which is guaranteed to not
        // have been cleared.
        if (immediate._destroyed) {
          outstandingQueue.head = immediate = prevImmediate._idleNext;
          continue;
        }

        immediate._destroyed = true;

        immediateInfo[kCount]--;
        if (immediate[kRefed])
          immediateInfo[kRefCount]--;
        immediate[kRefed] = null;

        prevImmediate = immediate;

        // TODO: AsyncContextFrame support
        // const priorContextFrame =
        //   AsyncContextFrame.exchange(immediate[async_context_frame]);

        // TODO: async_hooks emitBefore/emitAfter/emitDestroy
        // const asyncId = immediate[async_id_symbol];
        // emitBefore(asyncId, immediate[trigger_async_id_symbol], immediate);

        try {
          const argv = immediate._argv;
          if (!argv)
            immediate._onImmediate();
          else
            immediate._onImmediate(...argv);
        } finally {
          immediate._onImmediate = null;
          // TODO: emitDestroy(asyncId);
          outstandingQueue.head = immediate = immediate._idleNext;
        }

        // TODO: emitAfter(asyncId);
        // TODO: AsyncContextFrame.set(priorContextFrame);
      }

      if (queue === outstandingQueue)
        outstandingQueue.head = null;
      immediateInfo[kHasOutstanding] = 0;
    }

    function processTimers(now) {
      nextExpiry = Infinity;

      // TODO: Full timer processing with PriorityQueue
      // let list;
      // let ranAtLeastOneList = false;
      // while ((list = timerListQueue.peek()) != null) {
      //   if (list.expiry > now) {
      //     nextExpiry = list.expiry;
      //     return timeoutInfo[0] > 0 ? nextExpiry : -nextExpiry;
      //   }
      //   if (ranAtLeastOneList)
      //     runNextTicks_();
      //   else
      //     ranAtLeastOneList = true;
      //   listOnTimeout(list, now);
      // }
      return 0;
    }

    return {
      processImmediate,
      processTimers,
    };
  }

  // --------------------------------------------------------------------------
  // Promise Rejections (from lib/internal/process/promises.js)
  // --------------------------------------------------------------------------

  // *Must* match Environment::TickInfo::Fields in src/env.h.
  const kHasRejectionToWarn = 1;

  const kPromiseRejectWithNoHandler = 0;
  const kPromiseHandlerAddedAfterReject = 1;
  const kPromiseRejectAfterResolved = 2;
  const kPromiseResolveAfterResolved = 3;

  const maybeUnhandledPromises = new SafeWeakMap();
  let pendingUnhandledRejections = new SafeMap();
  const asyncHandledRejections = new FixedQueue();
  let lastPromiseId = 0;

  function setHasRejectionToWarn(value) {
    tickInfo[kHasRejectionToWarn] = value ? 1 : 0;
  }

  function hasRejectionToWarn() {
    return tickInfo[kHasRejectionToWarn] === 1;
  }

  function promiseRejectHandler(type, promise, reason) {
    switch (type) {
      case kPromiseRejectWithNoHandler:
        unhandledRejection(promise, reason);
        break;
      case kPromiseHandlerAddedAfterReject:
        handledRejection(promise);
        break;
      case kPromiseRejectAfterResolved:
      case kPromiseResolveAfterResolved:
        // Do nothing.
        break;
    }
  }

  function unhandledRejection(promise, reason) {
    pendingUnhandledRejections.set(promise, {
      reason,
      uid: ++lastPromiseId,
      warned: false,
      // TODO: domain support
      // domain: process.domain,
      // TODO: AsyncContextFrame support
      // contextFrame: AsyncContextFrame.current(),
    });
    setHasRejectionToWarn(true);
  }

  function handledRejection(promise) {
    if (pendingUnhandledRejections.has(promise)) {
      pendingUnhandledRejections.delete(promise);
      return;
    }
    const promiseInfo = maybeUnhandledPromises.get(promise);
    if (promiseInfo !== undefined) {
      maybeUnhandledPromises.delete(promise);
      if (promiseInfo.warned) {
        // TODO: Generate PromiseRejectionHandledWarning
        // asyncHandledRejections.push({ promise, warning });
        setHasRejectionToWarn(true);
      }
    }
  }

  // If this method returns true, we've executed user code or triggered
  // a warning to be emitted which requires the microtask and next tick
  // queues to be drained again.
  function processPromiseRejections() {
    let maybeScheduledTicksOrMicrotasks = !asyncHandledRejections.isEmpty();

    while (!asyncHandledRejections.isEmpty()) {
      const { promise, warning } = asyncHandledRejections.shift();
      // TODO: process.emit('rejectionHandled', promise)
      // if (!process.emit('rejectionHandled', promise)) {
      //   process.emitWarning(warning);
      // }
    }

    const pending = pendingUnhandledRejections;
    pendingUnhandledRejections = new SafeMap();

    for (const { 0: promise, 1: promiseInfo } of pending.entries()) {
      maybeUnhandledPromises.set(promise, promiseInfo);
      promiseInfo.warned = true;

      // TODO: async_hooks pushAsyncContext support
      // TODO: AsyncContextFrame support
      // TODO: unhandledRejectionsMode handling (strict/warn/throw/none)
      // For now, we use 'throw' mode by default (matching Node.js default).
      maybeScheduledTicksOrMicrotasks = true;
    }
    return maybeScheduledTicksOrMicrotasks ||
           pendingUnhandledRejections.size !== 0;
  }

  function listenForRejections() {
    // TODO: Wire up to V8's promise rejection callback
    // setPromiseRejectCallback(promiseRejectHandler);
  }

  function processRejections() {
    return processPromiseRejections();
  }

  // --------------------------------------------------------------------------
  // Exports
  // --------------------------------------------------------------------------

  const nodeCompat = {
    FixedQueue,
    setupTaskQueue,
    runNextTicks,
    setupTimers,
    getTimerCallbacks,
    processRejections,
    processPromiseRejections,
    listenForRejections,
    promiseRejectHandler,
    hasRejectionToWarn,
    setHasRejectionToWarn,
    nextTick,
    hasTickScheduled,
    setHasTickScheduled,
    ImmediateList,
    immediateQueue,
    immediateInfo,
    kRefed,
    TIMEOUT_MAX,
  };

  ObjectAssign(globalThis.__bootstrap, { nodeCompat });
})(globalThis);
