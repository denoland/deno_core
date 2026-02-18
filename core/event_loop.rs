// Copyright 2018-2025 the Deno authors. MIT license.

//! Phase-based event loop matching libuv's architecture.
//!
//! Each iteration of the event loop runs these phases **in order**:
//!
//! ```text
//! ┌───────────────────────────────┐
//! │         timers                │  ← Execute expired setTimeout/setInterval callbacks
//! ├───────────────────────────────┤
//! │     pending callbacks         │  ← I/O callbacks deferred from previous iteration
//! ├───────────────────────────────┤
//! │       idle / prepare          │  ← Internal hooks (op submission, GC hints)
//! ├───────────────────────────────┤
//! │          poll                 │  ← Poll async reactor for I/O completion, drive futures
//! ├───────────────────────────────┤
//! │         check                 │  ← setImmediate / queueMicrotask drain
//! ├───────────────────────────────┤
//! │      close callbacks          │  ← Resource cleanup callbacks
//! ├───────────────────────────────┤
//! │  (between iterations)         │  ← nextTick queue drain + microtask checkpoint
//! └───────────────────────────────┘
//! ```
//!
//! **Between every phase**: run microtask checkpoint (`scope.perform_microtask_checkpoint()`).

use std::collections::VecDeque;

/// Phase identifiers for the event loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventLoopPhase {
  /// Phase 1: Execute expired timer callbacks (setTimeout, setInterval).
  Timers,
  /// Phase 2: Execute I/O callbacks deferred from the previous iteration.
  PendingCallbacks,
  /// Phase 3a: Internal idle hooks.
  Idle,
  /// Phase 3b: Internal prepare hooks.
  Prepare,
  /// Phase 4: Poll the async reactor for I/O completion and drive futures.
  Poll,
  /// Phase 5: Execute setImmediate callbacks.
  Check,
  /// Phase 6: Execute close callbacks (resource cleanup).
  CloseCallbacks,
}

impl EventLoopPhase {
  /// Returns all phases in execution order.
  pub const fn all() -> &'static [EventLoopPhase] {
    &[
      EventLoopPhase::Timers,
      EventLoopPhase::PendingCallbacks,
      EventLoopPhase::Idle,
      EventLoopPhase::Prepare,
      EventLoopPhase::Poll,
      EventLoopPhase::Check,
      EventLoopPhase::CloseCallbacks,
    ]
  }
}

/// Callback type for phase observers (idle, prepare, check).
pub(crate) type PhaseCallback = Box<dyn FnMut()>;

/// Pending callback deferred from a previous iteration (I/O result).
#[allow(dead_code)]
pub(crate) struct PendingCallback {
  pub callback: Box<dyn FnOnce()>,
}

/// Close callback for resource cleanup.
pub(crate) struct CloseCallback {
  pub callback: Box<dyn FnOnce()>,
}

/// Phase-specific state for the event loop.
#[allow(dead_code)]
pub(crate) struct EventLoopPhases {
  /// Phase 2: Callbacks deferred from previous iteration.
  pub pending_callbacks: VecDeque<PendingCallback>,
  /// Phase 6: Close callbacks.
  pub close_callbacks: VecDeque<CloseCallback>,
  /// Phase 3a: Idle observers.
  pub idle_observers: Vec<PhaseCallback>,
  /// Phase 3b: Prepare observers.
  pub prepare_observers: Vec<PhaseCallback>,
  /// Phase 5: Check observers (setImmediate lives here).
  pub check_observers: Vec<PhaseCallback>,
}

impl Default for EventLoopPhases {
  fn default() -> Self {
    Self {
      pending_callbacks: VecDeque::new(),
      close_callbacks: VecDeque::new(),
      idle_observers: Vec::new(),
      prepare_observers: Vec::new(),
      check_observers: Vec::new(),
    }
  }
}

impl EventLoopPhases {
  /// Run all idle phase observers.
  #[allow(dead_code)]
  pub fn run_idle(&mut self) {
    for observer in self.idle_observers.iter_mut() {
      observer();
    }
  }

  /// Run all prepare phase observers.
  #[allow(dead_code)]
  pub fn run_prepare(&mut self) {
    for observer in self.prepare_observers.iter_mut() {
      observer();
    }
  }

  /// Run all check phase observers.
  #[allow(dead_code)]
  pub fn run_check(&mut self) {
    for observer in self.check_observers.iter_mut() {
      observer();
    }
  }

  /// Drain and run all pending callbacks.
  #[allow(dead_code)]
  pub fn run_pending_callbacks(&mut self) {
    while let Some(cb) = self.pending_callbacks.pop_front() {
      (cb.callback)();
    }
  }

  /// Drain and run all close callbacks.
  pub fn run_close_callbacks(&mut self) {
    while let Some(cb) = self.close_callbacks.pop_front() {
      (cb.callback)();
    }
  }

  /// Returns true if there is any pending work in the phase queues.
  #[allow(dead_code)]
  pub fn has_pending_work(&self) -> bool {
    !self.pending_callbacks.is_empty()
      || !self.close_callbacks.is_empty()
      || !self.idle_observers.is_empty()
      || !self.prepare_observers.is_empty()
      || !self.check_observers.is_empty()
  }
}

/// Run mode for the event loop, matching libuv's `uv_run_mode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub enum RunMode {
  /// Run the event loop until there are no more active handles/requests.
  Default = 0,
  /// Run a single iteration of the event loop.
  Once = 1,
  /// Run a single iteration without blocking for I/O.
  NoWait = 2,
}
