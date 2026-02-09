// Copyright 2018-2026 the Deno authors. MIT license.

//! Windows equivalent of `tokio::io::unix::AsyncFd` for IOCP handles.
//!
//! On Unix, libuv's backend fd (kqueue/epoll) can be registered with tokio's
//! reactor via `AsyncFd`. On Windows, libuv uses IOCP which cannot be nested
//! inside another IOCP. This module provides `AsyncHandle` which spawns a
//! lightweight watcher thread to detect IOCP completion packets and wake
//! tokio's reactor.
//!
//! # How it works
//!
//! 1. A watcher thread blocks on `GetQueuedCompletionStatusEx` on the
//!    target IOCP handle.
//! 2. When completion packets arrive, the thread **re-posts** them back
//!    to the same IOCP (so the real consumer can dequeue them) and wakes
//!    the registered `Waker`.
//! 3. The thread then **pauses** on a Windows Event until `clear_ready()`
//!    is called — this prevents the thread from immediately re-dequeuing
//!    the packets it just re-posted.
//! 4. The caller processes events (e.g., `uv_run(NoWait)`), then calls
//!    `clear_ready()` to resume the watcher thread.
//!
//! ```text
//! Watcher thread                     Main thread (tokio)
//! ──────────────                     ──────────────────
//! GQCS(iocp, INFINITE)               ...sleeping in tokio poll...
//!   ← packet arrives
//! PostQueuedCompletionStatus(×N)
//! waker.wake()
//! WaitForSingleObject(resume_event)   ← wakes up
//!                                     uv_run(NoWait)  // drains re-posted packets
//!                                     clear_ready()   // SetEvent(resume_event)
//!   ← resumed
//! back to GQCS                        ...sleeping in tokio poll...
//! ```

use std::ffi::c_void;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::task::Context;
use std::task::Poll;

use futures::task::AtomicWaker;

// ── Windows FFI ──────────────────────────────────────────────────────

type HANDLE = *mut c_void;
type BOOL = i32;
type DWORD = u32;
type ULONG_PTR = usize;

const INFINITE: DWORD = 0xFFFFFFFF;
const FALSE: BOOL = 0;
const TRUE: BOOL = 1;

#[repr(C)]
#[derive(Copy, Clone)]
struct OVERLAPPED_ENTRY {
  lp_completion_key: ULONG_PTR,
  lp_overlapped: *mut c_void,
  internal: ULONG_PTR,
  dw_number_of_bytes_transferred: DWORD,
}

unsafe impl Send for OVERLAPPED_ENTRY {}

extern "system" {
  fn GetQueuedCompletionStatusEx(
    completion_port: HANDLE,
    completion_port_entries: *mut OVERLAPPED_ENTRY,
    ul_count: DWORD,
    ul_num_entries_removed: *mut DWORD,
    dw_milliseconds: DWORD,
    f_alertable: BOOL,
  ) -> BOOL;

  fn PostQueuedCompletionStatus(
    completion_port: HANDLE,
    dw_number_of_bytes_transferred: DWORD,
    dw_completion_key: ULONG_PTR,
    lp_overlapped: *mut c_void,
  ) -> BOOL;

  fn CreateEventW(
    lp_event_attributes: *mut c_void,
    b_manual_reset: BOOL,
    b_initial_state: BOOL,
    lp_name: *const u16,
  ) -> HANDLE;

  fn SetEvent(h_event: HANDLE) -> BOOL;

  fn WaitForSingleObject(h_handle: HANDLE, dw_milliseconds: DWORD) -> DWORD;

  fn CloseHandle(h_object: HANDLE) -> BOOL;
}

// ── Shared state ─────────────────────────────────────────────────────

struct SharedState {
  /// Waker registered by the polling task.
  waker: AtomicWaker,
  /// Set by the watcher thread when packets are available.
  ready: AtomicBool,
  /// Auto-reset event used to pause/resume the watcher thread.
  /// Signaled = watcher may proceed to `GetQueuedCompletionStatusEx`.
  /// Non-signaled = watcher is paused (waiting for `clear_ready()`).
  resume_event: HANDLE,
  /// Set to `true` when `AsyncHandle` is dropped.
  shutdown: AtomicBool,
}

// HANDLE is Send-safe (it's just a pointer-sized value used across threads).
unsafe impl Send for SharedState {}
unsafe impl Sync for SharedState {}

// ── AsyncHandle ──────────────────────────────────────────────────────

/// Watches a Windows IOCP handle and wakes a tokio task when completion
/// packets are available. Analogous to `AsyncFd` on Unix.
pub struct AsyncHandle {
  state: Arc<SharedState>,
  thread: Option<std::thread::JoinHandle<()>>,
}

impl AsyncHandle {
  /// Create a new `AsyncHandle` watching the given IOCP handle.
  ///
  /// The IOCP handle is **not** owned — it will not be closed on drop.
  /// Spawns a background watcher thread immediately.
  pub fn new(iocp_handle: isize) -> Self {
    let resume_event = unsafe {
      CreateEventW(
        std::ptr::null_mut(),
        FALSE, // auto-reset: event resets after WaitForSingleObject returns
        TRUE,  // initial state: signaled (so first GQCS runs immediately)
        std::ptr::null(),
      )
    };
    assert!(
      !resume_event.is_null(),
      "CreateEventW failed: {}",
      std::io::Error::last_os_error()
    );

    let state = Arc::new(SharedState {
      waker: AtomicWaker::new(),
      ready: AtomicBool::new(false),
      resume_event,
      shutdown: AtomicBool::new(false),
    });

    let thread_state = state.clone();
    let thread = std::thread::Builder::new()
      .name("deno-iocp-watcher".into())
      .spawn(move || {
        watcher_thread_main(iocp_handle as HANDLE, thread_state);
      })
      .expect("failed to spawn IOCP watcher thread");

    AsyncHandle {
      state,
      thread: Some(thread),
    }
  }

  /// Check if the IOCP has pending completion packets.
  ///
  /// If ready, returns `Poll::Ready(())`. The caller should then process
  /// events (e.g., `uv_run(NoWait)`) and call [`clear_ready`] to resume
  /// the watcher thread.
  ///
  /// If not ready, registers `cx.waker()` for notification when packets
  /// arrive and returns `Poll::Pending`.
  pub fn poll_ready(&self, cx: &mut Context<'_>) -> Poll<()> {
    self.state.waker.register(cx.waker());
    if self.state.ready.load(Ordering::Acquire) {
      Poll::Ready(())
    } else {
      Poll::Pending
    }
  }

  /// Signal that the caller has finished processing events.
  ///
  /// This resumes the watcher thread so it can detect the next batch
  /// of completion packets. Must be called after `uv_run(NoWait)` (or
  /// equivalent) has drained the re-posted packets.
  pub fn clear_ready(&self) {
    if self.state.ready.swap(false, Ordering::AcqRel) {
      unsafe {
        SetEvent(self.state.resume_event);
      }
    }
  }
}

impl Drop for AsyncHandle {
  fn drop(&mut self) {
    // Signal shutdown and unblock the watcher thread.
    self.state.shutdown.store(true, Ordering::Release);
    unsafe {
      SetEvent(self.state.resume_event);
    }
    // The watcher thread may be blocked on GQCS. We use a short-timeout
    // loop in the thread (100ms) so it will notice the shutdown flag.
    if let Some(thread) = self.thread.take() {
      let _ = thread.join();
    }
    unsafe {
      CloseHandle(self.state.resume_event);
    }
  }
}

// ── Watcher thread ───────────────────────────────────────────────────

/// Maximum completion packets to dequeue per batch.
const BATCH_SIZE: usize = 64;

/// Timeout for `GetQueuedCompletionStatusEx` in milliseconds.
/// Using a finite timeout so the thread can check the shutdown flag
/// periodically and exit cleanly on drop.
const GQCS_TIMEOUT_MS: DWORD = 100;

fn watcher_thread_main(iocp: HANDLE, state: Arc<SharedState>) {
  let mut entries: [OVERLAPPED_ENTRY; BATCH_SIZE] =
    unsafe { std::mem::zeroed() };

  loop {
    // Wait for resume signal (auto-reset event).
    // On first iteration the event is pre-signaled.
    unsafe {
      WaitForSingleObject(state.resume_event, INFINITE);
    }

    if state.shutdown.load(Ordering::Acquire) {
      break;
    }

    // Block until IOCP has completion packets (or timeout).
    let mut num_entries: DWORD = 0;
    let ok = unsafe {
      GetQueuedCompletionStatusEx(
        iocp,
        entries.as_mut_ptr(),
        BATCH_SIZE as DWORD,
        &mut num_entries,
        GQCS_TIMEOUT_MS,
        FALSE,
      )
    };

    if state.shutdown.load(Ordering::Acquire) {
      // Re-post any dequeued entries before exiting so they aren't lost.
      if ok != 0 {
        repost_entries(iocp, &entries, num_entries as usize);
      }
      break;
    }

    if ok != 0 && num_entries > 0 {
      // Re-post all dequeued packets back to the IOCP.
      repost_entries(iocp, &entries, num_entries as usize);

      // Signal readiness and wake the tokio task.
      state.ready.store(true, Ordering::Release);
      state.waker.wake();
      // Loop back to WaitForSingleObject — will block until
      // clear_ready() calls SetEvent(resume_event).
    } else {
      // Timeout or error — no packets dequeued.
      // Resume immediately to try again.
      unsafe {
        SetEvent(state.resume_event);
      }
    }
  }
}

/// Re-post dequeued completion packets back to the IOCP.
fn repost_entries(
  iocp: HANDLE,
  entries: &[OVERLAPPED_ENTRY; BATCH_SIZE],
  count: usize,
) {
  for entry in &entries[..count] {
    unsafe {
      PostQueuedCompletionStatus(
        iocp,
        entry.dw_number_of_bytes_transferred,
        entry.lp_completion_key,
        entry.lp_overlapped,
      );
    }
  }
}
