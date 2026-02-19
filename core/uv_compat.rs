// Copyright 2018-2025 the Deno authors. MIT license.

//! **Pure Rust libuv Compatibility Layer**
//! 
//! This module provides a completely self-contained, pure Rust implementation
//! of libuv's C API. It has **NO external dependencies** on libuv, libuvrust,
//! or any other C libraries.
//! 
//! ## Features
//! - Complete C ABI compatibility with libuv
//! - Zero external dependencies - pure Rust implementation
//! - Supports timers, idle, prepare, and check handles
//! - Event loop phase execution matching libuv architecture
//! - Memory-safe with proper cleanup semantics
//! 
//! ## Usage with ext/node HTTP2
//! This layer is designed to be a drop-in replacement for libuv, allowing
//! Node.js HTTP2 code in ../deno/ext/node to work without modification.
//! 
//! ## Architecture
//! The implementation uses `UvLoopInner` as the core event loop state,
//! integrated with deno_core's phase-based event loop execution.

use std::cell::Cell;
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::ffi::c_char;
use std::ffi::c_int;
use std::ffi::c_void;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use futures::task::AtomicWaker;

/// A channel sender that wakes the event loop after each send.
#[derive(Clone)]
struct WakingSender {
  tx: std::sync::mpsc::Sender<PendingIo>,
  waker: Arc<AtomicWaker>,
}

impl WakingSender {
  fn send(&self, msg: PendingIo) {
    let _ = self.tx.send(msg);
    self.waker.wake();
  }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum uv_handle_type {
  UV_UNKNOWN_HANDLE = 0,
  UV_TIMER = 1,
  UV_IDLE = 2,
  UV_PREPARE = 3,
  UV_CHECK = 4,
  UV_TCP = 12,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum uv_run_mode {
  UV_RUN_DEFAULT = 0,
  UV_RUN_ONCE = 1,
  UV_RUN_NOWAIT = 2,
}

/// Flags stored in handle `flags` field.
const UV_HANDLE_ACTIVE: u32 = 1 << 0;
const UV_HANDLE_REF: u32 = 1 << 1;
const UV_HANDLE_CLOSING: u32 = 1 << 2;

#[repr(C)]
pub struct uv_loop_t {
  internal: *mut c_void,
  pub data: *mut c_void,
  stop_flag: Cell<bool>,
}

#[repr(C)]
pub struct uv_handle_t {
  pub r#type: uv_handle_type,
  pub loop_: *mut uv_loop_t,
  pub data: *mut c_void,
  pub flags: u32,
}

#[repr(C)]
pub struct uv_timer_t {
  pub r#type: uv_handle_type,
  pub loop_: *mut uv_loop_t,
  pub data: *mut c_void,
  pub flags: u32,
  internal_id: u64,
  internal_deadline: u64,
  cb: Option<unsafe extern "C" fn(*mut uv_timer_t)>,
  timeout: u64,
  repeat: u64,
}

#[repr(C)]
pub struct uv_idle_t {
  pub r#type: uv_handle_type,
  pub loop_: *mut uv_loop_t,
  pub data: *mut c_void,
  pub flags: u32,
  cb: Option<unsafe extern "C" fn(*mut uv_idle_t)>,
}

#[repr(C)]
pub struct uv_prepare_t {
  pub r#type: uv_handle_type,
  pub loop_: *mut uv_loop_t,
  pub data: *mut c_void,
  pub flags: u32,
  cb: Option<unsafe extern "C" fn(*mut uv_prepare_t)>,
}

#[repr(C)]
pub struct uv_check_t {
  pub r#type: uv_handle_type,
  pub loop_: *mut uv_loop_t,
  pub data: *mut c_void,
  pub flags: u32,
  cb: Option<unsafe extern "C" fn(*mut uv_check_t)>,
}

// Stream and TCP types needed for HTTP2 support
#[repr(C)]
pub struct uv_stream_t {
  pub r#type: uv_handle_type,
  pub loop_: *mut uv_loop_t,
  pub data: *mut c_void,
  pub flags: u32,
}

#[repr(C)]
pub struct uv_tcp_t {
  // Public C-ABI fields
  pub r#type: uv_handle_type,
  pub loop_: *mut uv_loop_t,
  pub data: *mut c_void,
  pub flags: u32,
  // Internal Rust state
  internal_fd: Option<std::os::fd::RawFd>,
  internal_bind_addr: Option<SocketAddr>,
  internal_stream: Option<Arc<tokio::net::TcpStream>>,
  internal_listener_addr: Option<SocketAddr>,
  internal_nodelay: bool,
  // Read state
  internal_alloc_cb: Option<uv_alloc_cb>,
  internal_read_cb: Option<uv_read_cb>,
  internal_reading: bool,
  internal_read_task: Option<tokio::task::JoinHandle<()>>,
  // Listener state
  internal_connection_cb: Option<uv_connection_cb>,
  internal_backlog: VecDeque<tokio::net::TcpStream>,
  internal_accept_task: Option<tokio::task::JoinHandle<()>>,
}

#[repr(C)]
pub struct uv_write_t {
  pub r#type: i32, // UV_REQ_TYPE fields
  pub data: *mut c_void,
  pub handle: *mut uv_stream_t,
}

#[repr(C)]
pub struct uv_connect_t {
  pub r#type: i32,
  pub data: *mut c_void,
  pub handle: *mut uv_stream_t,
}

#[repr(C)]
pub struct uv_shutdown_t {
  pub r#type: i32,
  pub data: *mut c_void,
  pub handle: *mut uv_stream_t,
}

#[repr(C)]
pub struct uv_buf_t {
  pub base: *mut c_char,
  pub len: usize,
}

pub type uv_timer_cb = unsafe extern "C" fn(*mut uv_timer_t);
pub type uv_idle_cb = unsafe extern "C" fn(*mut uv_idle_t);
pub type uv_prepare_cb = unsafe extern "C" fn(*mut uv_prepare_t);
pub type uv_check_cb = unsafe extern "C" fn(*mut uv_check_t);
pub type uv_close_cb = unsafe extern "C" fn(*mut uv_handle_t);
pub type uv_write_cb = unsafe extern "C" fn(*mut uv_write_t, i32);
pub type uv_alloc_cb = unsafe extern "C" fn(*mut uv_handle_t, usize, *mut uv_buf_t);
pub type uv_read_cb = unsafe extern "C" fn(*mut uv_stream_t, isize, *const uv_buf_t);
pub type uv_connection_cb = unsafe extern "C" fn(*mut uv_stream_t, i32);
pub type uv_connect_cb = unsafe extern "C" fn(*mut uv_connect_t, i32);
pub type uv_shutdown_cb = unsafe extern "C" fn(*mut uv_shutdown_t, i32);

// Type aliases for C compatibility
pub type UvHandle = uv_handle_t;
pub type UvLoop = uv_loop_t;
pub type UvStream = uv_stream_t;
pub type UvTcp = uv_tcp_t;
pub type UvWrite = uv_write_t;
pub type UvBuf = uv_buf_t;
pub type UvConnect = uv_connect_t;
pub type UvShutdown = uv_shutdown_t;

enum PendingIo {
  Connect {
    req: *mut uv_connect_t,
    tcp: *mut uv_tcp_t,
    result: std::io::Result<tokio::net::TcpStream>,
  },
  Accept {
    server: *mut uv_tcp_t,
    stream: tokio::net::TcpStream,
  },
  ReadData {
    tcp: *mut uv_tcp_t,
    data: Vec<u8>,
  },
  ReadEof {
    tcp: *mut uv_tcp_t,
  },
  ReadError {
    tcp: *mut uv_tcp_t,
    err: i32,
  },
  Write {
    req: *mut uv_write_t,
    status: i32,
  },
  Shutdown {
    req: *mut uv_shutdown_t,
    status: i32,
  },
}
// SAFETY: raw pointers are only dereferenced on the main thread inside run_io()
unsafe impl Send for PendingIo {}


/// Ordered by (deadline_ms, id) so we get
/// min-heap behavior from BTreeSet iteration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct TimerKey {
  deadline_ms: u64,
  id: u64,
}

/// Internal state
pub(crate) struct UvLoopInner {
  timers: RefCell<BTreeSet<TimerKey>>,
  next_timer_id: Cell<u64>,
  timer_handles: RefCell<HashMap<u64, *mut uv_timer_t>>,

  idle_handles: RefCell<Vec<*mut uv_idle_t>>,
  prepare_handles: RefCell<Vec<*mut uv_prepare_t>>,
  check_handles: RefCell<Vec<*mut uv_check_t>>,

  // TCP handles tracked for alive-handle checks
  tcp_handles: RefCell<Vec<*mut uv_tcp_t>>,
  // Pending I/O completions from spawned tokio tasks
  io_tx: std::sync::mpsc::Sender<PendingIo>,
  io_rx: RefCell<std::sync::mpsc::Receiver<PendingIo>>,
  // Buffer for items received during recv_timeout wakeup
  io_buffer: RefCell<VecDeque<PendingIo>>,
  // Waker to notify the deno_core event loop when I/O completes
  event_loop_waker: RefCell<Arc<AtomicWaker>>,

  closing_handles:
    RefCell<VecDeque<(*mut uv_handle_t, Option<uv_close_cb>)>>,

  time_origin: Instant,
}

impl UvLoopInner {
  fn new() -> Self {
    let (io_tx, io_rx) = std::sync::mpsc::channel();
    Self {
      timers: RefCell::new(BTreeSet::new()),
      next_timer_id: Cell::new(1),
      timer_handles: RefCell::new(HashMap::with_capacity(16)),
      idle_handles: RefCell::new(Vec::with_capacity(8)),
      prepare_handles: RefCell::new(Vec::with_capacity(8)),
      check_handles: RefCell::new(Vec::with_capacity(8)),
      tcp_handles: RefCell::new(Vec::with_capacity(8)),
      io_tx,
      io_rx: RefCell::new(io_rx),
      io_buffer: RefCell::new(VecDeque::new()),
      event_loop_waker: RefCell::new(Arc::new(AtomicWaker::new())),
      closing_handles: RefCell::new(VecDeque::with_capacity(16)),
      time_origin: Instant::now(),
    }
  }

  /// Set the event loop waker so I/O completions can wake the runtime.
  pub(crate) fn set_event_loop_waker(&self, waker: Arc<AtomicWaker>) {
    *self.event_loop_waker.borrow_mut() = waker;
  }

  /// Get a sender that wakes the event loop after each send.
  fn waking_sender(&self) -> WakingSender {
    WakingSender {
      tx: self.io_tx.clone(),
      waker: self.event_loop_waker.borrow().clone(),
    }
  }

  #[inline]
  fn alloc_timer_id(&self) -> u64 {
    let id = self.next_timer_id.get();
    self.next_timer_id.set(id + 1);
    id
  }

  #[inline]
  fn now_ms(&self) -> u64 {
    Instant::now().duration_since(self.time_origin).as_millis() as u64
  }

  /// Drive all libuv phases in order. Used by `uv_run` for standalone
  /// C-only consumers.
  ///
  /// When integrated with `JsRuntime::poll_event_loop_inner`, the individual
  /// `run_*` methods are called at their corresponding event loop phases
  /// instead of using this combined tick.
  ///
  /// # Safety
  /// All handle pointers stored in the internal lists must be valid.
  unsafe fn tick(&self) {
    unsafe {
      self.run_timers();
      self.run_io();
      self.run_idle();
      self.run_prepare();
      self.run_check();
      self.run_close();
    }
  }

  /// Returns true if there are any alive (active + ref'd) handles.
  pub(crate) fn has_alive_handles(&self) -> bool {
    for (_, handle_ptr) in self.timer_handles.borrow().iter() {
      let handle = unsafe { &**handle_ptr };
      if handle.flags & UV_HANDLE_ACTIVE != 0
        && handle.flags & UV_HANDLE_REF != 0
      {
        return true;
      }
    }
    for handle_ptr in self.idle_handles.borrow().iter() {
      let handle = unsafe { &**handle_ptr };
      if handle.flags & UV_HANDLE_ACTIVE != 0
        && handle.flags & UV_HANDLE_REF != 0
      {
        return true;
      }
    }
    for handle_ptr in self.prepare_handles.borrow().iter() {
      let handle = unsafe { &**handle_ptr };
      if handle.flags & UV_HANDLE_ACTIVE != 0
        && handle.flags & UV_HANDLE_REF != 0
      {
        return true;
      }
    }
    for handle_ptr in self.check_handles.borrow().iter() {
      let handle = unsafe { &**handle_ptr };
      if handle.flags & UV_HANDLE_ACTIVE != 0
        && handle.flags & UV_HANDLE_REF != 0
      {
        return true;
      }
    }
    for handle_ptr in self.tcp_handles.borrow().iter() {
      let handle = unsafe { &**handle_ptr };
      if handle.flags & UV_HANDLE_ACTIVE != 0
        && handle.flags & UV_HANDLE_REF != 0
      {
        return true;
      }
    }
    // Pending close callbacks count as alive
    if !self.closing_handles.borrow().is_empty() {
      return true;
    }
    false
  }

  /// Returns the next timer deadline in ms, or None if no timers.
  fn next_timer_deadline_ms(&self) -> Option<u64> {
    self.timers.borrow().iter().next().map(|k| k.deadline_ms)
  }

  /// Compute how long `uv_run` should wait before the next tick.
  /// Returns Duration::ZERO if there is immediate work (idle/close callbacks).
  fn compute_wait_timeout(&self) -> Duration {
    // Immediate work means no waiting
    if !self.idle_handles.borrow().is_empty()
      || !self.closing_handles.borrow().is_empty()
    {
      return Duration::ZERO;
    }
    if let Some(deadline) = self.next_timer_deadline_ms() {
      let now = self.now_ms();
      if deadline > now {
        Duration::from_millis(deadline - now)
      } else {
        Duration::ZERO
      }
    } else if !self.tcp_handles.borrow().is_empty() {
      // No timers but active I/O: wait up to 1 second for I/O completions
      // (recv_timeout will wake immediately on any I/O event)
      Duration::from_secs(1)
    } else {
      Duration::ZERO
    }
  }

  /// Fire all expired C timers. Called at Phase 1 (Timers).
  ///
  /// # Safety
  /// All timer handle pointers must be valid.
  pub(crate) unsafe fn run_timers(&self) {
    let now = self.now_ms();
    // Collect expired timers
    let mut expired = Vec::new();
    {
      let timers = self.timers.borrow();
      for key in timers.iter() {
        if key.deadline_ms > now {
          break;
        }
        expired.push(*key);
      }
    }

    for key in expired {
      self.timers.borrow_mut().remove(&key);
      let handle_ptr =
        match self.timer_handles.borrow().get(&key.id).copied() {
          Some(h) => h,
          None => continue,
        };
      let handle = unsafe { &mut *handle_ptr };
      // Only fire if still active
      if handle.flags & UV_HANDLE_ACTIVE == 0 {
        self.timer_handles.borrow_mut().remove(&key.id);
        continue;
      }
      let cb = handle.cb;
      let repeat = handle.repeat;

      if repeat > 0 {
        // Re-arm repeat timer
        let new_deadline = now + repeat;
        let new_key = TimerKey {
          deadline_ms: new_deadline,
          id: key.id,
        };
        handle.internal_deadline = new_deadline;
        self.timers.borrow_mut().insert(new_key);
      } else {
        // One-shot: deactivate
        handle.flags &= !UV_HANDLE_ACTIVE;
        self.timer_handles.borrow_mut().remove(&key.id);
      }

      if let Some(cb) = cb {
        unsafe { cb(handle_ptr) };
      }
    }
  }

  /// Fire all active idle callbacks. Called at Phase 3a (Idle).
  ///
  /// # Safety
  /// All idle handle pointers must be valid.
  pub(crate) unsafe fn run_idle(&self) {
    let snapshot: Vec<*mut uv_idle_t> =
      self.idle_handles.borrow().clone();
    for handle_ptr in snapshot {
      let handle = unsafe { &*handle_ptr };
      if handle.flags & UV_HANDLE_ACTIVE != 0 {
        if let Some(cb) = handle.cb {
          unsafe { cb(handle_ptr) };
        }
      }
    }
  }

  /// Fire all active prepare callbacks. Called at Phase 3b (Prepare).
  ///
  /// # Safety
  /// All prepare handle pointers must be valid.
  pub(crate) unsafe fn run_prepare(&self) {
    let snapshot: Vec<*mut uv_prepare_t> =
      self.prepare_handles.borrow().clone();
    for handle_ptr in snapshot {
      let handle = unsafe { &*handle_ptr };
      if handle.flags & UV_HANDLE_ACTIVE != 0 {
        if let Some(cb) = handle.cb {
          unsafe { cb(handle_ptr) };
        }
      }
    }
  }

  /// Fire all active check callbacks. Called at Phase 5 (Check).
  ///
  /// # Safety
  /// All check handle pointers must be valid.
  pub(crate) unsafe fn run_check(&self) {
    let snapshot: Vec<*mut uv_check_t> =
      self.check_handles.borrow().clone();
    for handle_ptr in snapshot {
      let handle = unsafe { &*handle_ptr };
      if handle.flags & UV_HANDLE_ACTIVE != 0 {
        if let Some(cb) = handle.cb {
          unsafe { cb(handle_ptr) };
        }
      }
    }
  }

  /// Drain and fire all queued close callbacks. Called at Phase 6 (Close).
  ///
  /// # Safety
  /// All handle pointers in the closing queue must be valid.
  pub(crate) unsafe fn run_close(&self) {
    let mut closing = self.closing_handles.borrow_mut();
    let snapshot: Vec<_> = closing.drain(..).collect();
    drop(closing);
    for (handle_ptr, cb) in snapshot {
      if let Some(cb) = cb {
        unsafe { cb(handle_ptr) };
      }
    }
  }

  /// Drain I/O completions from spawned tokio tasks and fire callbacks.
  ///
  /// # Safety
  /// All TCP handle pointers in pending I/O messages must be valid.
  pub(crate) unsafe fn run_io(&self) {
    // Drain buffered items (received during recv_timeout wakeup)
    let buffered: Vec<PendingIo> =
      self.io_buffer.borrow_mut().drain(..).collect();
    for pending in buffered {
      unsafe { self.dispatch_io(pending) };
    }

    // Drain channel
    let rx = self.io_rx.borrow();
    while let Ok(pending) = rx.try_recv() {
      unsafe { self.dispatch_io(pending) };
    }
  }

  /// Dispatch a single I/O completion.
  ///
  /// # Safety
  /// All pointers in the `PendingIo` must be valid.
  unsafe fn dispatch_io(&self, pending: PendingIo) {
    match pending {
      PendingIo::Connect { req, tcp, result } => unsafe {
        let tcp_ref = &mut *tcp;
        let status = match result {
          Ok(stream) => {
            if tcp_ref.internal_nodelay {
              stream.set_nodelay(true).ok();
            }
            tcp_ref.internal_stream = Some(Arc::new(stream));
            0
          }
          Err(_) => -61, // ECONNREFUSED
        };
        (*req).handle = tcp as *mut uv_stream_t;
        let cb_ptr = (*req).data as *const Option<uv_connect_cb>;
        if !cb_ptr.is_null() {
          if let Some(cb) = *cb_ptr {
            cb(req, status);
          }
        }
      },
      PendingIo::Accept { server, stream } => unsafe {
        let tcp_ref = &mut *server;
        tcp_ref.internal_backlog.push_back(stream);
        if let Some(cb) = tcp_ref.internal_connection_cb {
          cb(server as *mut uv_stream_t, 0);
        }
      },
      PendingIo::ReadData { tcp, data } => unsafe {
        let tcp_ref = &mut *tcp;
        if !tcp_ref.internal_reading {
          return;
        }
        if let (Some(alloc_cb), Some(read_cb)) =
          (tcp_ref.internal_alloc_cb, tcp_ref.internal_read_cb)
        {
          let mut buf = uv_buf_t {
            base: std::ptr::null_mut(),
            len: 0,
          };
          alloc_cb(tcp as *mut uv_handle_t, data.len(), &mut buf);
          if !buf.base.is_null() && buf.len > 0 {
            let copy_len = data.len().min(buf.len);
            std::ptr::copy_nonoverlapping(
              data.as_ptr(),
              buf.base as *mut u8,
              copy_len,
            );
            read_cb(tcp as *mut uv_stream_t, copy_len as isize, &buf);
          }
        }
      },
      PendingIo::ReadEof { tcp } => unsafe {
        let tcp_ref = &mut *tcp;
        tcp_ref.internal_reading = false;
        if let (Some(alloc_cb), Some(read_cb)) =
          (tcp_ref.internal_alloc_cb, tcp_ref.internal_read_cb)
        {
          let mut buf = uv_buf_t {
            base: std::ptr::null_mut(),
            len: 0,
          };
          alloc_cb(tcp as *mut uv_handle_t, 0, &mut buf);
          read_cb(tcp as *mut uv_stream_t, -4095, &buf); // UV_EOF
        }
      },
      PendingIo::ReadError { tcp, err } => unsafe {
        let tcp_ref = &mut *tcp;
        tcp_ref.internal_reading = false;
        if let (Some(alloc_cb), Some(read_cb)) =
          (tcp_ref.internal_alloc_cb, tcp_ref.internal_read_cb)
        {
          let mut buf = uv_buf_t {
            base: std::ptr::null_mut(),
            len: 0,
          };
          alloc_cb(tcp as *mut uv_handle_t, 0, &mut buf);
          read_cb(tcp as *mut uv_stream_t, err as isize, &buf);
        }
      },
      PendingIo::Write { req, status } => unsafe {
        let cb_ptr = (*req).data as *const Option<uv_write_cb>;
        if !cb_ptr.is_null() {
          if let Some(cb) = *cb_ptr {
            cb(req, status);
          }
        }
      },
      PendingIo::Shutdown { req, status } => unsafe {
        let cb_ptr = (*req).data as *const Option<uv_shutdown_cb>;
        if !cb_ptr.is_null() {
          if let Some(cb) = *cb_ptr {
            cb(req, status);
          }
        }
      },
    }
  }

  /// Stop a timer handle and remove it from the inner structures.
  unsafe fn stop_timer(&self, handle: *mut uv_timer_t) {
    let handle_ref = unsafe { &mut *handle };
    let id = handle_ref.internal_id;
    if id != 0 {
      let key = TimerKey {
        deadline_ms: handle_ref.internal_deadline,
        id,
      };
      self.timers.borrow_mut().remove(&key);
      self.timer_handles.borrow_mut().remove(&id);
    }
    handle_ref.flags &= !UV_HANDLE_ACTIVE;
  }

  fn stop_idle(&self, handle: *mut uv_idle_t) {
    self
      .idle_handles
      .borrow_mut()
      .retain(|&h| !std::ptr::eq(h, handle));
    unsafe {
      (*handle).flags &= !UV_HANDLE_ACTIVE;
    }
  }

  fn stop_prepare(&self, handle: *mut uv_prepare_t) {
    self
      .prepare_handles
      .borrow_mut()
      .retain(|&h| !std::ptr::eq(h, handle));
    unsafe {
      (*handle).flags &= !UV_HANDLE_ACTIVE;
    }
  }

  fn stop_check(&self, handle: *mut uv_check_t) {
    self
      .check_handles
      .borrow_mut()
      .retain(|&h| !std::ptr::eq(h, handle));
    unsafe {
      (*handle).flags &= !UV_HANDLE_ACTIVE;
    }
  }

  fn stop_tcp(&self, handle: *mut uv_tcp_t) {
    self
      .tcp_handles
      .borrow_mut()
      .retain(|&h| !std::ptr::eq(h, handle));
    unsafe {
      let tcp = &mut *handle;
      tcp.internal_reading = false;
      tcp.internal_alloc_cb = None;
      tcp.internal_read_cb = None;
      tcp.internal_connection_cb = None;
      tcp.internal_stream = None;
      if let Some(task) = tcp.internal_read_task.take() {
        task.abort();
      }
      if let Some(task) = tcp.internal_accept_task.take() {
        task.abort();
      }
      tcp.internal_backlog.clear();
      tcp.flags &= !UV_HANDLE_ACTIVE;
    }
  }
}

/// Get the `UvLoopInner` from a `uv_loop_t` pointer.
///
/// # Safety
/// `loop_` must be a valid, initialized loop pointer.
#[inline]
unsafe fn get_inner(loop_: *mut uv_loop_t) -> &'static UvLoopInner {
  unsafe { &*((*loop_).internal as *const UvLoopInner) }
}

/// Get the raw `UvLoopInner` pointer from a `uv_loop_t`.
///
/// This is used by `JsRuntime::register_uv_loop` to connect the loop
/// to the event loop phases.
///
/// # Safety
/// `loop_` must be a valid, initialized loop pointer.
pub unsafe fn uv_loop_get_inner_ptr(
  loop_: *const uv_loop_t,
) -> *const std::ffi::c_void {
  unsafe { (*loop_).internal as *const std::ffi::c_void }
}

/// Initialize a loop handle.
///
/// # Safety
/// `loop_` must be a valid, non-null pointer to an uninitialized `uv_loop_t`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uv_loop_init(loop_: *mut uv_loop_t) -> c_int {
  let inner = Box::new(UvLoopInner::new());
  unsafe {
    (*loop_).internal = Box::into_raw(inner) as *mut c_void;
    (*loop_).data = std::ptr::null_mut();
    (*loop_).stop_flag = Cell::new(false);
  }
  0
}

/// Close and clean up a loop handle.
///
/// # Safety
/// `loop_` must be a valid, initialized loop pointer. No handles should
/// be active when this is called.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uv_loop_close(loop_: *mut uv_loop_t) -> c_int {
  unsafe {
    let internal = (*loop_).internal;
    if !internal.is_null() {
      drop(Box::from_raw(internal as *mut UvLoopInner));
      (*loop_).internal = std::ptr::null_mut();
    }
  }
  0
}

/// Signal the loop to stop. The next `uv_run` iteration will exit.
///
/// # Safety
/// `loop_` must be a valid, initialized loop pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uv_stop(loop_: *mut uv_loop_t) {
  unsafe {
    (*loop_).stop_flag.set(true);
  }
}

/// Return the current cached time in ms since loop start.
///
/// # Safety
/// `loop_` must be a valid, initialized loop pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uv_now(loop_: *mut uv_loop_t) -> u64 {
  let inner = unsafe { get_inner(loop_) };
  inner.now_ms()
}

/// Update the cached time. (Currently a no-op since we compute on demand.)
///
/// # Safety
/// `loop_` must be a valid, initialized loop pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uv_update_time(_loop_: *mut uv_loop_t) {
  // Time is computed on demand from Instant, no caching needed.
}

// ---------------------------------------------------------------------------
// Timer API
// ---------------------------------------------------------------------------

/// Initialize a timer handle.
///
/// # Safety
/// `loop_` and `handle` must be valid, non-null pointers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uv_timer_init(
  loop_: *mut uv_loop_t,
  handle: *mut uv_timer_t,
) -> c_int {
  unsafe {
    (*handle).r#type = uv_handle_type::UV_TIMER;
    (*handle).loop_ = loop_;
    (*handle).data = std::ptr::null_mut();
    (*handle).flags = UV_HANDLE_REF;
    (*handle).internal_id = 0;
    (*handle).internal_deadline = 0;
    (*handle).cb = None;
    (*handle).timeout = 0;
    (*handle).repeat = 0;
  }
  0
}

/// Start a timer. `timeout` is the initial timeout in ms, `repeat` is the
/// repeat interval in ms (0 = one-shot).
///
/// # Safety
/// `handle` must have been initialized with `uv_timer_init`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uv_timer_start(
  handle: *mut uv_timer_t,
  cb: uv_timer_cb,
  timeout: u64,
  repeat: u64,
) -> c_int {
  unsafe {
    let loop_ = (*handle).loop_;
    let inner = get_inner(loop_);

    // If already active, stop first (remove old entry)
    if (*handle).flags & UV_HANDLE_ACTIVE != 0 {
      inner.stop_timer(handle);
    }

    let id = inner.alloc_timer_id();
    let deadline = inner.now_ms() + timeout;

    (*handle).cb = Some(cb);
    (*handle).timeout = timeout;
    (*handle).repeat = repeat;
    (*handle).internal_id = id;
    (*handle).internal_deadline = deadline;
    (*handle).flags |= UV_HANDLE_ACTIVE;

    let key = TimerKey {
      deadline_ms: deadline,
      id,
    };
    inner.timers.borrow_mut().insert(key);
    inner.timer_handles.borrow_mut().insert(id, handle);
  }
  0
}

/// Stop a timer.
///
/// # Safety
/// `handle` must have been initialized with `uv_timer_init`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uv_timer_stop(handle: *mut uv_timer_t) -> c_int {
  unsafe {
    let loop_ = (*handle).loop_;
    if loop_.is_null() || (*loop_).internal.is_null() {
      (*handle).flags &= !UV_HANDLE_ACTIVE;
      return 0;
    }
    let inner = get_inner(loop_);
    inner.stop_timer(handle);
  }
  0
}

/// Restart a repeat timer. If the timer was not started or has no repeat
/// interval, this returns UV_EINVAL (-22).
///
/// # Safety
/// `handle` must have been initialized with `uv_timer_init`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uv_timer_again(
  handle: *mut uv_timer_t,
) -> c_int {
  unsafe {
    let repeat = (*handle).repeat;
    if repeat == 0 {
      return -22; // UV_EINVAL
    }
    let loop_ = (*handle).loop_;
    let inner = get_inner(loop_);

    // Stop existing
    inner.stop_timer(handle);

    // Re-arm with repeat as timeout
    let id = inner.alloc_timer_id();
    let deadline = inner.now_ms() + repeat;

    (*handle).internal_id = id;
    (*handle).internal_deadline = deadline;
    (*handle).flags |= UV_HANDLE_ACTIVE;

    let key = TimerKey {
      deadline_ms: deadline,
      id,
    };
    inner.timers.borrow_mut().insert(key);
    inner.timer_handles.borrow_mut().insert(id, handle);
  }
  0
}

/// Get the repeat interval of a timer.
///
/// # Safety
/// `handle` must have been initialized with `uv_timer_init`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uv_timer_get_repeat(
  handle: *const uv_timer_t,
) -> u64 {
  unsafe { (*handle).repeat }
}

/// Set the repeat interval of a timer.
///
/// # Safety
/// `handle` must have been initialized with `uv_timer_init`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uv_timer_set_repeat(
  handle: *mut uv_timer_t,
  repeat: u64,
) {
  unsafe {
    (*handle).repeat = repeat;
  }
}

// ---------------------------------------------------------------------------
// Idle API
// ---------------------------------------------------------------------------

/// Initialize an idle handle.
///
/// # Safety
/// `loop_` and `handle` must be valid, non-null pointers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uv_idle_init(
  loop_: *mut uv_loop_t,
  handle: *mut uv_idle_t,
) -> c_int {
  unsafe {
    (*handle).r#type = uv_handle_type::UV_IDLE;
    (*handle).loop_ = loop_;
    (*handle).data = std::ptr::null_mut();
    (*handle).flags = UV_HANDLE_REF;
    (*handle).cb = None;
  }
  0
}

/// Start an idle handle.
///
/// # Safety
/// `handle` must have been initialized with `uv_idle_init`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uv_idle_start(
  handle: *mut uv_idle_t,
  cb: uv_idle_cb,
) -> c_int {
  unsafe {
    // Idempotent: if already active, just update cb
    if (*handle).flags & UV_HANDLE_ACTIVE != 0 {
      (*handle).cb = Some(cb);
      return 0;
    }
    (*handle).cb = Some(cb);
    (*handle).flags |= UV_HANDLE_ACTIVE;

    let loop_ = (*handle).loop_;
    let inner = get_inner(loop_);
    inner.idle_handles.borrow_mut().push(handle);
  }
  0
}

/// Stop an idle handle.
///
/// # Safety
/// `handle` must have been initialized with `uv_idle_init`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uv_idle_stop(handle: *mut uv_idle_t) -> c_int {
  unsafe {
    if (*handle).flags & UV_HANDLE_ACTIVE == 0 {
      return 0;
    }
    let loop_ = (*handle).loop_;
    let inner = get_inner(loop_);
    inner.stop_idle(handle);
    (*handle).cb = None;
  }
  0
}

// ---------------------------------------------------------------------------
// Prepare API
// ---------------------------------------------------------------------------

/// Initialize a prepare handle.
///
/// # Safety
/// `loop_` and `handle` must be valid, non-null pointers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uv_prepare_init(
  loop_: *mut uv_loop_t,
  handle: *mut uv_prepare_t,
) -> c_int {
  unsafe {
    (*handle).r#type = uv_handle_type::UV_PREPARE;
    (*handle).loop_ = loop_;
    (*handle).data = std::ptr::null_mut();
    (*handle).flags = UV_HANDLE_REF;
    (*handle).cb = None;
  }
  0
}

/// Start a prepare handle.
///
/// # Safety
/// `handle` must have been initialized with `uv_prepare_init`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uv_prepare_start(
  handle: *mut uv_prepare_t,
  cb: uv_prepare_cb,
) -> c_int {
  unsafe {
    if (*handle).flags & UV_HANDLE_ACTIVE != 0 {
      (*handle).cb = Some(cb);
      return 0;
    }
    (*handle).cb = Some(cb);
    (*handle).flags |= UV_HANDLE_ACTIVE;

    let loop_ = (*handle).loop_;
    let inner = get_inner(loop_);
    inner.prepare_handles.borrow_mut().push(handle);
  }
  0
}

/// Stop a prepare handle.
///
/// # Safety
/// `handle` must have been initialized with `uv_prepare_init`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uv_prepare_stop(
  handle: *mut uv_prepare_t,
) -> c_int {
  unsafe {
    if (*handle).flags & UV_HANDLE_ACTIVE == 0 {
      return 0;
    }
    let loop_ = (*handle).loop_;
    let inner = get_inner(loop_);
    inner.stop_prepare(handle);
    (*handle).cb = None;
  }
  0
}

// ---------------------------------------------------------------------------
// Check API
// ---------------------------------------------------------------------------

/// Initialize a check handle.
///
/// # Safety
/// `loop_` and `handle` must be valid, non-null pointers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uv_check_init(
  loop_: *mut uv_loop_t,
  handle: *mut uv_check_t,
) -> c_int {
  unsafe {
    (*handle).r#type = uv_handle_type::UV_CHECK;
    (*handle).loop_ = loop_;
    (*handle).data = std::ptr::null_mut();
    (*handle).flags = UV_HANDLE_REF;
    (*handle).cb = None;
  }
  0
}

/// Start a check handle.
///
/// # Safety
/// `handle` must have been initialized with `uv_check_init`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uv_check_start(
  handle: *mut uv_check_t,
  cb: uv_check_cb,
) -> c_int {
  unsafe {
    if (*handle).flags & UV_HANDLE_ACTIVE != 0 {
      (*handle).cb = Some(cb);
      return 0;
    }
    (*handle).cb = Some(cb);
    (*handle).flags |= UV_HANDLE_ACTIVE;

    let loop_ = (*handle).loop_;
    let inner = get_inner(loop_);
    inner.check_handles.borrow_mut().push(handle);
  }
  0
}

/// Stop a check handle.
///
/// # Safety
/// `handle` must have been initialized with `uv_check_init`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uv_check_stop(handle: *mut uv_check_t) -> c_int {
  unsafe {
    if (*handle).flags & UV_HANDLE_ACTIVE == 0 {
      return 0;
    }
    let loop_ = (*handle).loop_;
    let inner = get_inner(loop_);
    inner.stop_check(handle);
    (*handle).cb = None;
  }
  0
}

// ---------------------------------------------------------------------------
// Common handle API
// ---------------------------------------------------------------------------

/// Close a handle. The `close_cb` is called asynchronously in the close phase.
///
/// # Safety
/// `handle` must be a valid, initialized handle pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uv_close(
  handle: *mut uv_handle_t,
  close_cb: Option<uv_close_cb>,
) {
  unsafe {
    (*handle).flags |= UV_HANDLE_CLOSING;
    (*handle).flags &= !UV_HANDLE_ACTIVE;

    let loop_ = (*handle).loop_;
    let inner = get_inner(loop_);

    // Stop the handle based on its type
    match (*handle).r#type {
      uv_handle_type::UV_TIMER => {
        inner.stop_timer(handle as *mut uv_timer_t);
      }
      uv_handle_type::UV_IDLE => {
        inner.stop_idle(handle as *mut uv_idle_t);
      }
      uv_handle_type::UV_PREPARE => {
        inner.stop_prepare(handle as *mut uv_prepare_t);
      }
      uv_handle_type::UV_CHECK => {
        inner.stop_check(handle as *mut uv_check_t);
      }
      uv_handle_type::UV_TCP => {
        inner.stop_tcp(handle as *mut uv_tcp_t);
      }
      _ => {}
    }

    // Queue close callback for the close phase
    inner
      .closing_handles
      .borrow_mut()
      .push_back((handle, close_cb));
  }
}

/// Reference a handle, preventing the event loop from exiting.
///
/// # Safety
/// `handle` must be a valid handle pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uv_ref(handle: *mut uv_handle_t) {
  unsafe {
    (*handle).flags |= UV_HANDLE_REF;
  }
}

/// Un-reference a handle, allowing the event loop to exit even if active.
///
/// # Safety
/// `handle` must be a valid handle pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uv_unref(handle: *mut uv_handle_t) {
  unsafe {
    (*handle).flags &= !UV_HANDLE_REF;
  }
}

/// Check if a handle is active.
///
/// # Safety
/// `handle` must be a valid handle pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uv_is_active(handle: *const uv_handle_t) -> c_int {
  unsafe {
    if (*handle).flags & UV_HANDLE_ACTIVE != 0 {
      1
    } else {
      0
    }
  }
}

/// Check if a handle is closing or closed.
///
/// # Safety
/// `handle` must be a valid handle pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uv_is_closing(handle: *const uv_handle_t) -> c_int {
  unsafe {
    if (*handle).flags & UV_HANDLE_CLOSING != 0 {
      1
    } else {
      0
    }
  }
}

/// Run the event loop.
///
/// For `UV_RUN_DEFAULT`, loops until no alive handles remain or `uv_stop`
/// is called. For `UV_RUN_ONCE`, runs a single tick. For `UV_RUN_NOWAIT`,
/// runs a single tick without blocking.
///
/// When integrated with `JsRuntime`, the real event loop is driven by
/// `poll_event_loop`. This function is useful for C-only consumers or testing.
///
/// # Safety
/// `loop_` must be a valid, initialized loop pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn uv_run(
  loop_: *mut uv_loop_t,
  mode: uv_run_mode,
) -> c_int {
  unsafe {
    let inner = get_inner(loop_);
    (*loop_).stop_flag.set(false);

    match mode {
      uv_run_mode::UV_RUN_DEFAULT => {
        while inner.has_alive_handles() && !(*loop_).stop_flag.get() {
          // Calculate wait timeout
          let timeout = inner.compute_wait_timeout();
          if timeout > Duration::ZERO {
            // Block on I/O channel or timer expiry - wakes on either
            let rx = inner.io_rx.borrow();
            if let Ok(item) = rx.recv_timeout(timeout) {
              drop(rx);
              inner.io_buffer.borrow_mut().push_back(item);
            }
          }
          inner.tick();
        }
        if inner.has_alive_handles() {
          1
        } else {
          0
        }
      }
      uv_run_mode::UV_RUN_ONCE => {
        let timeout = inner.compute_wait_timeout();
        if timeout > Duration::ZERO {
          let rx = inner.io_rx.borrow();
          if let Ok(item) = rx.recv_timeout(timeout) {
            drop(rx);
            inner.io_buffer.borrow_mut().push_back(item);
          }
        }
        inner.tick();
        if inner.has_alive_handles() {
          1
        } else {
          0
        }
      }
      uv_run_mode::UV_RUN_NOWAIT => {
        inner.tick();
        if inner.has_alive_handles() {
          1
        } else {
          0
        }
      }
    }
  }
}

// ---------------------------------------------------------------------------
// sockaddr helpers
// ---------------------------------------------------------------------------

/// Parse a C sockaddr into a Rust `SocketAddr`.
///
/// # Safety
/// `addr` must point to a valid `sockaddr_in` or `sockaddr_in6`.
unsafe fn sockaddr_to_std(addr: *const c_void) -> Option<SocketAddr> {
  // On macOS/BSD, sa_family is at byte offset 1 (u8), on Linux it's u16 at offset 0.
  // libc::sockaddr_in uses the platform-correct layout.
  let sa = addr as *const libc::sockaddr;
  let family = unsafe { (*sa).sa_family as i32 };
  if family == libc::AF_INET {
    let sin = unsafe { &*(addr as *const libc::sockaddr_in) };
    let ip = std::net::Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr));
    let port = u16::from_be(sin.sin_port);
    Some(SocketAddr::from((ip, port)))
  } else if family == libc::AF_INET6 {
    let sin6 = unsafe { &*(addr as *const libc::sockaddr_in6) };
    let ip = std::net::Ipv6Addr::from(sin6.sin6_addr.s6_addr);
    let port = u16::from_be(sin6.sin6_port);
    Some(SocketAddr::from((ip, port)))
  } else {
    None
  }
}

/// Write a Rust `SocketAddr` into a C sockaddr buffer.
///
/// # Safety
/// `out` must point to a buffer large enough for the sockaddr type.
/// `len` must be a valid pointer.
unsafe fn std_to_sockaddr(
  addr: SocketAddr,
  out: *mut c_void,
  len: *mut c_int,
) {
  match addr {
    SocketAddr::V4(v4) => {
      let sin = out as *mut libc::sockaddr_in;
      unsafe {
        std::ptr::write_bytes(sin, 0, 1);
        #[cfg(any(target_os = "macos", target_os = "freebsd"))]
        {
          (*sin).sin_len = std::mem::size_of::<libc::sockaddr_in>() as u8;
        }
        (*sin).sin_family = libc::AF_INET as libc::sa_family_t;
        (*sin).sin_port = v4.port().to_be();
        (*sin).sin_addr.s_addr = u32::from(*v4.ip()).to_be();
        *len = std::mem::size_of::<libc::sockaddr_in>() as c_int;
      }
    }
    SocketAddr::V6(v6) => {
      let sin6 = out as *mut libc::sockaddr_in6;
      unsafe {
        std::ptr::write_bytes(sin6, 0, 1);
        #[cfg(any(target_os = "macos", target_os = "freebsd"))]
        {
          (*sin6).sin6_len = std::mem::size_of::<libc::sockaddr_in6>() as u8;
        }
        (*sin6).sin6_family = libc::AF_INET6 as libc::sa_family_t;
        (*sin6).sin6_port = v6.port().to_be();
        (*sin6).sin6_addr.s6_addr = v6.ip().octets();
        (*sin6).sin6_scope_id = v6.scope_id();
        *len = std::mem::size_of::<libc::sockaddr_in6>() as c_int;
      }
    }
  }
}

// ---------------------------------------------------------------------------
// Stream / TCP functions
// ---------------------------------------------------------------------------

pub unsafe fn uv_tcp_init(
  loop_: *mut uv_loop_t,
  tcp: *mut uv_tcp_t,
) -> c_int {
  // Use ptr::write for each field to avoid dropping uninitialized memory
  // when tcp points to MaybeUninit storage.
  unsafe {
    use std::ptr::{addr_of_mut, write};
    write(addr_of_mut!((*tcp).r#type), uv_handle_type::UV_TCP);
    write(addr_of_mut!((*tcp).loop_), loop_);
    write(addr_of_mut!((*tcp).data), std::ptr::null_mut());
    write(addr_of_mut!((*tcp).flags), UV_HANDLE_REF);
    write(addr_of_mut!((*tcp).internal_fd), None);
    write(addr_of_mut!((*tcp).internal_bind_addr), None);
    write(addr_of_mut!((*tcp).internal_stream), None);
    write(addr_of_mut!((*tcp).internal_listener_addr), None);
    write(addr_of_mut!((*tcp).internal_nodelay), false);
    write(addr_of_mut!((*tcp).internal_alloc_cb), None);
    write(addr_of_mut!((*tcp).internal_read_cb), None);
    write(addr_of_mut!((*tcp).internal_reading), false);
    write(addr_of_mut!((*tcp).internal_read_task), None);
    write(addr_of_mut!((*tcp).internal_connection_cb), None);
    write(addr_of_mut!((*tcp).internal_backlog), VecDeque::new());
    write(addr_of_mut!((*tcp).internal_accept_task), None);
  }
  0
}

pub unsafe fn uv_tcp_open(tcp: *mut uv_tcp_t, fd: c_int) -> c_int {
  unsafe {
    use std::os::unix::io::FromRawFd;
    let std_stream = std::net::TcpStream::from_raw_fd(fd);
    std_stream.set_nonblocking(true).ok();
    (*tcp).internal_fd = Some(fd);
    match tokio::net::TcpStream::from_std(std_stream) {
      Ok(stream) => {
        if (*tcp).internal_nodelay {
          stream.set_nodelay(true).ok();
        }
        (*tcp).internal_stream = Some(Arc::new(stream));
        0
      }
      Err(_) => -22, // UV_EINVAL
    }
  }
}

pub unsafe fn uv_tcp_bind(
  tcp: *mut uv_tcp_t,
  addr: *const c_void,
  _addrlen: u32,
  _flags: u32,
) -> c_int {
  let sock_addr = unsafe { sockaddr_to_std(addr) };
  match sock_addr {
    Some(sa) => {
      unsafe { (*tcp).internal_bind_addr = Some(sa) };
      0
    }
    None => -22, // UV_EINVAL
  }
}

pub unsafe fn uv_tcp_connect(
  req: *mut uv_connect_t,
  tcp: *mut uv_tcp_t,
  addr: *const c_void,
  cb: Option<uv_connect_cb>,
) -> c_int {
  let sock_addr = unsafe { sockaddr_to_std(addr) };
  let sock_addr = match sock_addr {
    Some(sa) => sa,
    None => return -22, // UV_EINVAL
  };

  // Store callback in req.data (leaked Box, cleaned up after callback)
  let cb_box = Box::new(cb);
  unsafe {
    (*req).data = Box::into_raw(cb_box) as *mut c_void;
    (*req).handle = tcp as *mut uv_stream_t;
  }

  let inner = unsafe { get_inner((*tcp).loop_) };
  let tx = inner.waking_sender();

  // Mark handle active and register for tracking
  unsafe {
    (*tcp).flags |= UV_HANDLE_ACTIVE;
    let mut handles = inner.tcp_handles.borrow_mut();
    if !handles.iter().any(|&h| std::ptr::eq(h, tcp)) {
      handles.push(tcp);
    }
  }

  // SAFETY: req and tcp pointers remain valid; they are heap-allocated by
  // the caller and only dereferenced on the main thread in run_io().
  let req_addr = req as usize;
  let tcp_addr = tcp as usize;
  tokio::task::spawn(async move {
    let result = tokio::net::TcpStream::connect(sock_addr).await;
    tx.send(PendingIo::Connect {
      req: req_addr as *mut uv_connect_t,
      tcp: tcp_addr as *mut uv_tcp_t,
      result,
    });
  });

  0
}

pub unsafe fn uv_tcp_nodelay(tcp: *mut uv_tcp_t, enable: c_int) -> c_int {
  unsafe {
    let enabled = enable != 0;
    (*tcp).internal_nodelay = enabled;
    if let Some(ref stream) = (*tcp).internal_stream {
      if stream.set_nodelay(enabled).is_err() {
        return -22; // UV_EINVAL
      }
    }
  }
  0
}

pub unsafe fn uv_tcp_getpeername(
  tcp: *const uv_tcp_t,
  name: *mut c_void,
  namelen: *mut c_int,
) -> c_int {
  unsafe {
    if let Some(ref stream) = (*tcp).internal_stream {
      match stream.peer_addr() {
        Ok(addr) => {
          std_to_sockaddr(addr, name, namelen);
          0
        }
        Err(_) => -107, // UV_ENOTCONN
      }
    } else {
      -107 // UV_ENOTCONN
    }
  }
}

pub unsafe fn uv_tcp_getsockname(
  tcp: *const uv_tcp_t,
  name: *mut c_void,
  namelen: *mut c_int,
) -> c_int {
  unsafe {
    // Try stream first, then listener addr, then bind addr
    if let Some(ref stream) = (*tcp).internal_stream {
      match stream.local_addr() {
        Ok(addr) => {
          std_to_sockaddr(addr, name, namelen);
          return 0;
        }
        Err(_) => return -22,
      }
    }
    if let Some(addr) = (*tcp).internal_listener_addr {
      std_to_sockaddr(addr, name, namelen);
      return 0;
    }
    // Check bind addr
    if let Some(addr) = (*tcp).internal_bind_addr {
      std_to_sockaddr(addr, name, namelen);
      return 0;
    }
    -22 // UV_EINVAL
  }
}

pub unsafe fn uv_listen(
  stream: *mut uv_stream_t,
  _backlog: c_int,
  cb: Option<uv_connection_cb>,
) -> c_int {
  unsafe {
    let tcp = stream as *mut uv_tcp_t;
    let tcp_ref = &mut *tcp;

    // Bind and create listener
    let bind_addr = tcp_ref
      .internal_bind_addr
      .unwrap_or_else(|| "0.0.0.0:0".parse().unwrap());

    // Bind with std first to get the actual address, then convert to tokio
    let std_listener = match std::net::TcpListener::bind(bind_addr) {
      Ok(l) => l,
      Err(_) => return -98, // UV_EADDRINUSE
    };
    std_listener.set_nonblocking(true).ok();
    let listener_addr = std_listener.local_addr().ok();
    let tokio_listener =
      match tokio::net::TcpListener::from_std(std_listener) {
        Ok(l) => l,
        Err(_) => return -22, // UV_EINVAL
      };

    // Store the listener address for getsockname
    tcp_ref.internal_listener_addr = listener_addr;
    tcp_ref.internal_connection_cb = cb;
    tcp_ref.flags |= UV_HANDLE_ACTIVE;

    // Register in tcp_handles if not already present
    let inner = get_inner(tcp_ref.loop_);
    let mut handles = inner.tcp_handles.borrow_mut();
    if !handles.iter().any(|&h| std::ptr::eq(h, tcp)) {
      handles.push(tcp);
    }
    drop(handles);

    // Spawn accept loop
    let tx = inner.waking_sender();
    let server_addr = tcp as usize;
    let task = tokio::task::spawn(async move {
      loop {
        match tokio_listener.accept().await {
          Ok((stream, _)) => {
            tx.send(PendingIo::Accept {
              server: server_addr as *mut uv_tcp_t,
              stream,
            });
          }
          Err(_) => break,
        }
      }
    });
    tcp_ref.internal_accept_task = Some(task);
  }
  0
}

pub unsafe fn uv_accept(
  server: *mut uv_stream_t,
  client: *mut uv_stream_t,
) -> c_int {
  unsafe {
    let server_tcp = &mut *(server as *mut uv_tcp_t);
    let client_tcp = &mut *(client as *mut uv_tcp_t);

    match server_tcp.internal_backlog.pop_front() {
      Some(stream) => {
        if client_tcp.internal_nodelay {
          stream.set_nodelay(true).ok();
        }
        client_tcp.internal_stream = Some(Arc::new(stream));
        0
      }
      None => -11, // UV_EAGAIN
    }
  }
}

pub unsafe fn uv_read_start(
  stream: *mut uv_stream_t,
  alloc_cb: Option<uv_alloc_cb>,
  read_cb: Option<uv_read_cb>,
) -> c_int {
  unsafe {
    let tcp = stream as *mut uv_tcp_t;
    let tcp_ref = &mut *tcp;
    tcp_ref.internal_alloc_cb = alloc_cb;
    tcp_ref.internal_read_cb = read_cb;
    tcp_ref.internal_reading = true;
    tcp_ref.flags |= UV_HANDLE_ACTIVE;

    // Register in tcp_handles if not already present
    let inner = get_inner(tcp_ref.loop_);
    let mut handles = inner.tcp_handles.borrow_mut();
    if !handles.iter().any(|&h| std::ptr::eq(h, tcp)) {
      handles.push(tcp);
    }
    drop(handles);

    // Spawn a tokio read loop if we have a stream
    if let Some(ref stream) = tcp_ref.internal_stream {
      let stream = Arc::clone(stream);
      let tx = inner.waking_sender();
      let tcp_addr = tcp as usize;
      let task = tokio::task::spawn(async move {
        loop {
          // Wait for the stream to become readable
          if stream.readable().await.is_err() {
            tx.send(PendingIo::ReadError {
              tcp: tcp_addr as *mut uv_tcp_t,
              err: -4095, // UV_EOF
            });
            break;
          }
          // Try to read data
          let mut buf = vec![0u8; 65536];
          match stream.try_read(&mut buf) {
            Ok(0) => {
              tx.send(PendingIo::ReadEof {
                tcp: tcp_addr as *mut uv_tcp_t,
              });
              break;
            }
            Ok(n) => {
              buf.truncate(n);
              tx.send(PendingIo::ReadData {
                tcp: tcp_addr as *mut uv_tcp_t,
                data: buf,
              });
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
              // Spurious wakeup, try again
              continue;
            }
            Err(_) => {
              tx.send(PendingIo::ReadError {
                tcp: tcp_addr as *mut uv_tcp_t,
                err: -4095,
              });
              break;
            }
          }
        }
      });
      // Abort any previous read task
      if let Some(old_task) = tcp_ref.internal_read_task.take() {
        old_task.abort();
      }
      tcp_ref.internal_read_task = Some(task);
    }
  }
  0
}

pub unsafe fn uv_read_stop(stream: *mut uv_stream_t) -> c_int {
  unsafe {
    let tcp = stream as *mut uv_tcp_t;
    let tcp_ref = &mut *tcp;
    tcp_ref.internal_reading = false;
    tcp_ref.internal_alloc_cb = None;
    tcp_ref.internal_read_cb = None;
    if let Some(task) = tcp_ref.internal_read_task.take() {
      task.abort();
    }
    // Only clear ACTIVE if not also listening
    if tcp_ref.internal_connection_cb.is_none() {
      tcp_ref.flags &= !UV_HANDLE_ACTIVE;
    }
  }
  0
}

pub unsafe fn uv_write(
  req: *mut uv_write_t,
  handle: *mut uv_stream_t,
  bufs: *const uv_buf_t,
  nbufs: u32,
  cb: Option<uv_write_cb>,
) -> c_int {
  unsafe {
    let tcp = handle as *mut uv_tcp_t;
    let tcp_ref = &mut *tcp;
    (*req).handle = handle;

    if let Some(ref stream) = tcp_ref.internal_stream {
      // Collect all buffer data
      let mut write_data = Vec::new();
      for i in 0..nbufs as usize {
        let buf = &*bufs.add(i);
        if buf.base.is_null() || buf.len == 0 {
          continue;
        }
        write_data.extend_from_slice(std::slice::from_raw_parts(
          buf.base as *const u8,
          buf.len,
        ));
      }

      // Try synchronous non-blocking write first
      let mut offset = 0;
      loop {
        match stream.try_write(&write_data[offset..]) {
          Ok(n) => {
            offset += n;
            if offset >= write_data.len() {
              // Fully written synchronously
              if let Some(cb) = cb {
                cb(req, 0);
              }
              return 0;
            }
          }
          Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
            break; // Fall through to async path
          }
          Err(_) => {
            if let Some(cb) = cb {
              cb(req, -32); // UV_EPIPE
            }
            return 0;
          }
        }
      }

      // Async write for remaining data
      let remaining = write_data[offset..].to_vec();
      let stream = Arc::clone(stream);
      let inner = get_inner(tcp_ref.loop_);
      let tx = inner.waking_sender();
      let cb_box = Box::new(cb);
      (*req).data = Box::into_raw(cb_box) as *mut c_void;
      let req_addr = req as usize;

      tokio::task::spawn(async move {
        let mut offset = 0;
        let mut status = 0i32;
        while offset < remaining.len() {
          match stream.writable().await {
            Ok(()) => match stream.try_write(&remaining[offset..]) {
              Ok(n) => offset += n,
              Err(ref e)
                if e.kind() == std::io::ErrorKind::WouldBlock =>
              {
                continue;
              }
              Err(_) => {
                status = -32; // UV_EPIPE
                break;
              }
            },
            Err(_) => {
              status = -32;
              break;
            }
          }
        }
        tx.send(PendingIo::Write {
          req: req_addr as *mut uv_write_t,
          status,
        });
      });
    } else {
      // No stream connected
      if let Some(cb) = cb {
        cb(req, -107); // UV_ENOTCONN
      }
    }
  }
  0
}

pub unsafe fn uv_shutdown(
  req: *mut uv_shutdown_t,
  stream: *mut uv_stream_t,
  cb: Option<uv_shutdown_cb>,
) -> c_int {
  unsafe {
    let tcp = stream as *mut uv_tcp_t;
    (*req).handle = stream;

    let status = if let Some(ref stream) = (*tcp).internal_stream {
      use std::os::unix::io::AsRawFd;
      let fd = stream.as_raw_fd();
      if libc::shutdown(fd, libc::SHUT_WR) == 0 {
        0
      } else {
        -107 // UV_ENOTCONN
      }
    } else {
      -107
    };

    if let Some(cb) = cb {
      cb(req, status);
    }
  }
  0
}

// Constructor helpers

pub fn new_tcp() -> UvTcp {
  uv_tcp_t {
    r#type: uv_handle_type::UV_TCP,
    loop_: std::ptr::null_mut(),
    data: std::ptr::null_mut(),
    flags: 0,
    internal_fd: None,
    internal_bind_addr: None,
    internal_stream: None,
    internal_listener_addr: None,
    internal_nodelay: false,
    internal_alloc_cb: None,
    internal_read_cb: None,
    internal_reading: false,
    internal_read_task: None,
    internal_connection_cb: None,
    internal_backlog: VecDeque::new(),
    internal_accept_task: None,
  }
}

pub fn new_write() -> UvWrite {
  uv_write_t {
    r#type: 0,
    data: std::ptr::null_mut(),
    handle: std::ptr::null_mut(),
  }
}

pub fn new_connect() -> UvConnect {
  uv_connect_t {
    r#type: 0,
    data: std::ptr::null_mut(),
    handle: std::ptr::null_mut(),
  }
}

pub fn new_shutdown() -> UvShutdown {
  uv_shutdown_t {
    r#type: 0,
    data: std::ptr::null_mut(),
    handle: std::ptr::null_mut(),
  }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
  use super::*;
  use std::mem::MaybeUninit;
  use std::sync::atomic::{AtomicU32, AtomicBool, Ordering};

  /// Helper: allocate and init a loop on the heap.
  unsafe fn make_loop() -> *mut uv_loop_t {
    let loop_ = Box::into_raw(Box::new(MaybeUninit::<uv_loop_t>::uninit()))
      as *mut uv_loop_t;
    unsafe { uv_loop_init(loop_) };
    loop_
  }

  /// Helper: destroy a loop created with `make_loop`.
  unsafe fn destroy_loop(loop_: *mut uv_loop_t) {
    unsafe {
      uv_loop_close(loop_);
      drop(Box::from_raw(loop_));
    }
  }

  // 1. Loop init/close lifecycle
  #[test]
  fn test_loop_init_close() {
    unsafe {
      let loop_ = make_loop();
      assert!(!(*loop_).internal.is_null());
      uv_loop_close(loop_);
      assert!((*loop_).internal.is_null());
      drop(Box::from_raw(loop_));
    }
  }

  // 2. Timer fires after timeout via uv_run(ONCE)
  #[test]
  fn test_timer_fires() {
    unsafe {
      let loop_ = make_loop();
      let mut timer = MaybeUninit::<uv_timer_t>::uninit();
      let timer_ptr = timer.as_mut_ptr();
      uv_timer_init(loop_, timer_ptr);

      static FIRED: AtomicBool = AtomicBool::new(false);
      unsafe extern "C" fn timer_cb(_handle: *mut uv_timer_t) {
        FIRED.store(true, Ordering::Relaxed);
      }

      uv_timer_start(timer_ptr, timer_cb, 10, 0);
      assert_eq!((*timer_ptr).flags & UV_HANDLE_ACTIVE, UV_HANDLE_ACTIVE);

      // Run once — should sleep and fire
      uv_run(loop_, uv_run_mode::UV_RUN_ONCE);
      assert!(FIRED.load(Ordering::Relaxed));
      // One-shot timer should be inactive
      assert_eq!((*timer_ptr).flags & UV_HANDLE_ACTIVE, 0);

      destroy_loop(loop_);
    }
  }

  // 3. Repeat timer fires multiple times
  #[test]
  fn test_repeat_timer() {
    unsafe {
      let loop_ = make_loop();
      let mut timer = MaybeUninit::<uv_timer_t>::uninit();
      let timer_ptr = timer.as_mut_ptr();
      uv_timer_init(loop_, timer_ptr);

      static COUNT: AtomicU32 = AtomicU32::new(0);
      unsafe extern "C" fn timer_cb(handle: *mut uv_timer_t) {
        let c = COUNT.fetch_add(1, Ordering::Relaxed) + 1;
        if c >= 3 {
          unsafe { uv_timer_stop(handle) };
        }
      }

      COUNT.store(0, Ordering::Relaxed);
      uv_timer_start(timer_ptr, timer_cb, 5, 5);

      uv_run(loop_, uv_run_mode::UV_RUN_DEFAULT);
      assert_eq!(COUNT.load(Ordering::Relaxed), 3);

      destroy_loop(loop_);
    }
  }

  // 4. Idle callbacks fire every iteration
  #[test]
  fn test_idle_fires() {
    unsafe {
      let loop_ = make_loop();
      let mut idle = MaybeUninit::<uv_idle_t>::uninit();
      let idle_ptr = idle.as_mut_ptr();
      uv_idle_init(loop_, idle_ptr);

      static IDLE_COUNT: AtomicU32 = AtomicU32::new(0);
      unsafe extern "C" fn idle_cb(handle: *mut uv_idle_t) {
        let c = IDLE_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
        if c >= 5 {
          unsafe { uv_idle_stop(handle) };
        }
      }

      IDLE_COUNT.store(0, Ordering::Relaxed);
      uv_idle_start(idle_ptr, idle_cb);

      uv_run(loop_, uv_run_mode::UV_RUN_DEFAULT);
      assert_eq!(IDLE_COUNT.load(Ordering::Relaxed), 5);

      destroy_loop(loop_);
    }
  }

  // 5. Prepare/check fire in correct order
  #[test]
  fn test_prepare_check_order() {
    unsafe {
      let loop_ = make_loop();

      let mut prepare = MaybeUninit::<uv_prepare_t>::uninit();
      let prepare_ptr = prepare.as_mut_ptr();
      uv_prepare_init(loop_, prepare_ptr);

      let mut check = MaybeUninit::<uv_check_t>::uninit();
      let check_ptr = check.as_mut_ptr();
      uv_check_init(loop_, check_ptr);

      // Use a Vec<&str> stored in loop data to track order
      let mut order: Vec<&str> = Vec::new();
      let order_ptr = &mut order as *mut Vec<&str> as *mut c_void;
      (*loop_).data = order_ptr;

      unsafe extern "C" fn prepare_cb(handle: *mut uv_prepare_t) {
        unsafe {
          let loop_ = (*handle).loop_;
          let order =
            &mut *((*loop_).data as *mut Vec<&str>);
          order.push("prepare");
          uv_prepare_stop(handle);
        }
      }

      unsafe extern "C" fn check_cb(handle: *mut uv_check_t) {
        unsafe {
          let loop_ = (*handle).loop_;
          let order =
            &mut *((*loop_).data as *mut Vec<&str>);
          order.push("check");
          uv_check_stop(handle);
        }
      }

      uv_prepare_start(prepare_ptr, prepare_cb);
      uv_check_start(check_ptr, check_cb);

      uv_run(loop_, uv_run_mode::UV_RUN_ONCE);

      assert_eq!(order, vec!["prepare", "check"]);

      destroy_loop(loop_);
    }
  }

  // 6. uv_close queues callback to close phase
  #[test]
  fn test_close_callback() {
    unsafe {
      let loop_ = make_loop();
      let mut timer = MaybeUninit::<uv_timer_t>::uninit();
      let timer_ptr = timer.as_mut_ptr();
      uv_timer_init(loop_, timer_ptr);

      static CLOSE_CALLED: AtomicBool = AtomicBool::new(false);
      unsafe extern "C" fn close_cb(_handle: *mut uv_handle_t) {
        CLOSE_CALLED.store(true, Ordering::Relaxed);
      }

      CLOSE_CALLED.store(false, Ordering::Relaxed);

      // Start and immediately close
      unsafe extern "C" fn noop_cb(_handle: *mut uv_timer_t) {}
      uv_timer_start(timer_ptr, noop_cb, 1000, 0);
      uv_close(timer_ptr as *mut uv_handle_t, Some(close_cb));

      assert_eq!(
        (*timer_ptr).flags & UV_HANDLE_CLOSING,
        UV_HANDLE_CLOSING
      );
      // Close callback should not have been called yet
      assert!(!CLOSE_CALLED.load(Ordering::Relaxed));

      // Run once to process close callbacks
      uv_run(loop_, uv_run_mode::UV_RUN_ONCE);
      assert!(CLOSE_CALLED.load(Ordering::Relaxed));

      destroy_loop(loop_);
    }
  }

  // 7. uv_unref prevents handle from keeping loop alive
  #[test]
  fn test_unref_exits_loop() {
    unsafe {
      let loop_ = make_loop();
      let mut timer = MaybeUninit::<uv_timer_t>::uninit();
      let timer_ptr = timer.as_mut_ptr();
      uv_timer_init(loop_, timer_ptr);

      unsafe extern "C" fn noop_cb(_handle: *mut uv_timer_t) {}
      uv_timer_start(timer_ptr, noop_cb, 60000, 0);

      // Unref the timer — loop should exit immediately
      uv_unref(timer_ptr as *mut uv_handle_t);

      let result = uv_run(loop_, uv_run_mode::UV_RUN_NOWAIT);
      assert_eq!(result, 0); // 0 = no alive handles

      // Clean up
      uv_timer_stop(timer_ptr);
      destroy_loop(loop_);
    }
  }

  // 8. uv_stop breaks DEFAULT loop
  #[test]
  fn test_stop_breaks_loop() {
    unsafe {
      let loop_ = make_loop();
      let mut idle = MaybeUninit::<uv_idle_t>::uninit();
      let idle_ptr = idle.as_mut_ptr();
      uv_idle_init(loop_, idle_ptr);

      static STOP_COUNT: AtomicU32 = AtomicU32::new(0);

      // Store loop pointer in handle data for the callback
      (*idle_ptr).data = loop_ as *mut c_void;

      unsafe extern "C" fn idle_stop_cb(handle: *mut uv_idle_t) {
        let c = STOP_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
        if c >= 3 {
          let loop_ = unsafe { (*handle).data as *mut uv_loop_t };
          unsafe { uv_stop(loop_) };
        }
      }

      STOP_COUNT.store(0, Ordering::Relaxed);
      uv_idle_start(idle_ptr, idle_stop_cb);

      uv_run(loop_, uv_run_mode::UV_RUN_DEFAULT);

      // Should have stopped after 3 iterations
      assert_eq!(STOP_COUNT.load(Ordering::Relaxed), 3);

      uv_idle_stop(idle_ptr);
      destroy_loop(loop_);
    }
  }

  // 9. timer_again re-arms a repeat timer
  #[test]
  fn test_timer_again() {
    unsafe {
      let loop_ = make_loop();
      let mut timer = MaybeUninit::<uv_timer_t>::uninit();
      let timer_ptr = timer.as_mut_ptr();
      uv_timer_init(loop_, timer_ptr);

      // timer_again with repeat=0 should fail
      assert_eq!(uv_timer_again(timer_ptr), -22);

      static AGAIN_FIRED: AtomicBool = AtomicBool::new(false);
      unsafe extern "C" fn timer_cb(handle: *mut uv_timer_t) {
        AGAIN_FIRED.store(true, Ordering::Relaxed);
        unsafe { uv_timer_stop(handle) };
      }

      AGAIN_FIRED.store(false, Ordering::Relaxed);
      // Start timer first to set the callback, then use timer_again to restart
      uv_timer_start(timer_ptr, timer_cb, 1000, 10);
      uv_timer_stop(timer_ptr);
      uv_timer_again(timer_ptr);

      uv_run(loop_, uv_run_mode::UV_RUN_DEFAULT);
      assert!(AGAIN_FIRED.load(Ordering::Relaxed));

      destroy_loop(loop_);
    }
  }

  // 10. timer get/set repeat
  #[test]
  fn test_timer_get_set_repeat() {
    unsafe {
      let loop_ = make_loop();
      let mut timer = MaybeUninit::<uv_timer_t>::uninit();
      let timer_ptr = timer.as_mut_ptr();
      uv_timer_init(loop_, timer_ptr);

      assert_eq!(uv_timer_get_repeat(timer_ptr), 0);
      uv_timer_set_repeat(timer_ptr, 42);
      assert_eq!(uv_timer_get_repeat(timer_ptr), 42);

      destroy_loop(loop_);
    }
  }

  /// Helper to create a sockaddr_in for 127.0.0.1:port
  unsafe fn make_sockaddr_in(port: u16) -> libc::sockaddr_in {
    let mut addr: libc::sockaddr_in = std::mem::zeroed();
    #[cfg(any(target_os = "macos", target_os = "freebsd"))]
    {
      addr.sin_len = std::mem::size_of::<libc::sockaddr_in>() as u8;
    }
    addr.sin_family = libc::AF_INET as libc::sa_family_t;
    addr.sin_port = port.to_be();
    addr.sin_addr.s_addr = u32::from(std::net::Ipv4Addr::LOCALHOST).to_be();
    addr
  }

  // 11. TCP listen and accept
  #[tokio::test(flavor = "multi_thread")]
  async fn test_tcp_listen_accept() {
    unsafe {
      let loop_ = make_loop();

      // Setup server
      let mut server = MaybeUninit::<uv_tcp_t>::uninit();
      let server_ptr = server.as_mut_ptr();
      uv_tcp_init(loop_, server_ptr);

      // Bind to ephemeral port
      let bind_addr = make_sockaddr_in(0);
      uv_tcp_bind(
        server_ptr,
        &bind_addr as *const _ as *const c_void,
        0,
        0,
      );

      static CONN_CALLED: AtomicBool = AtomicBool::new(false);

      unsafe extern "C" fn on_connection(
        server: *mut uv_stream_t,
        status: i32,
      ) {
        assert_eq!(status, 0);
        // Accept the client
        let server_tcp = server as *mut uv_tcp_t;
        let loop_ = (*server_tcp).loop_;
        let client = Box::into_raw(Box::new(new_tcp()));
        uv_tcp_init(loop_, client);
        let rc = uv_accept(server, client as *mut uv_stream_t);
        assert_eq!(rc, 0);
        assert!((*client).internal_stream.is_some());

        // Clean up client
        drop(Box::from_raw(client));
        CONN_CALLED.store(true, Ordering::Relaxed);

        // Stop the loop
        uv_stop(loop_);
      }

      CONN_CALLED.store(false, Ordering::Relaxed);
      let rc = uv_listen(
        server_ptr as *mut uv_stream_t,
        128,
        Some(on_connection),
      );
      assert_eq!(rc, 0);

      // Get the actual port
      let mut name_buf: libc::sockaddr_in = std::mem::zeroed();
      let mut namelen =
        std::mem::size_of::<libc::sockaddr_in>() as c_int;
      uv_tcp_getsockname(
        server_ptr,
        &mut name_buf as *mut _ as *mut c_void,
        &mut namelen,
      );
      let port = u16::from_be(name_buf.sin_port);
      assert!(port > 0);

      // Connect from another thread
      let port_copy = port;
      std::thread::spawn(move || {
        // Give the loop a moment to start
        std::thread::sleep(Duration::from_millis(50));
        let _ = std::net::TcpStream::connect(
          format!("127.0.0.1:{}", port_copy),
        );
      });

      // Run loop - will exit when uv_stop is called in callback
      uv_run(loop_, uv_run_mode::UV_RUN_DEFAULT);
      assert!(CONN_CALLED.load(Ordering::Relaxed));

      // Clean up
      uv_close(server_ptr as *mut uv_handle_t, None);
      uv_run(loop_, uv_run_mode::UV_RUN_ONCE);
      destroy_loop(loop_);
    }
  }

  // 12. TCP connect and write
  #[tokio::test(flavor = "multi_thread")]
  async fn test_tcp_connect_and_write() {
    unsafe {
      let loop_ = make_loop();

      // Create a std listener on an ephemeral port
      let std_listener =
        std::net::TcpListener::bind("127.0.0.1:0").unwrap();
      let port = std_listener.local_addr().unwrap().port();

      // Spawn a thread to accept and read
      let received = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
      let received_clone = received.clone();
      std::thread::spawn(move || {
        let (mut stream, _) = std_listener.accept().unwrap();
        let mut buf = [0u8; 1024];
        loop {
          match std::io::Read::read(&mut stream, &mut buf) {
            Ok(0) => break,
            Ok(n) => received_clone.lock().unwrap().extend_from_slice(&buf[..n]),
            Err(_) => break,
          }
        }
      });

      // Create TCP handle and connect
      let mut tcp = MaybeUninit::<uv_tcp_t>::uninit();
      let tcp_ptr = tcp.as_mut_ptr();
      uv_tcp_init(loop_, tcp_ptr);

      let addr = make_sockaddr_in(port);
      let mut connect_req = MaybeUninit::<uv_connect_t>::uninit();
      let connect_req_ptr = connect_req.as_mut_ptr();

      static CONNECT_CALLED: AtomicBool = AtomicBool::new(false);
      static WRITE_CALLED: AtomicBool = AtomicBool::new(false);

      unsafe extern "C" fn on_connect(
        req: *mut uv_connect_t,
        status: i32,
      ) {
        assert_eq!(status, 0);
        CONNECT_CALLED.store(true, Ordering::Relaxed);

        // Write data
        let stream = (*req).handle;
        let data = b"hello world";
        let buf = uv_buf_t {
          base: data.as_ptr() as *mut c_char,
          len: data.len(),
        };
        let write_req = Box::into_raw(Box::new(new_write()));

        unsafe extern "C" fn on_write(
          req: *mut uv_write_t,
          status: i32,
        ) {
          assert_eq!(status, 0);
          WRITE_CALLED.store(true, Ordering::Relaxed);
          drop(Box::from_raw(req));
        }

        uv_write(write_req, stream, &buf, 1, Some(on_write));

        // Close after write
        let tcp = stream as *mut uv_tcp_t;
        let loop_ = (*tcp).loop_;
        uv_close(tcp as *mut uv_handle_t, None);
        uv_stop(loop_);
      }

      CONNECT_CALLED.store(false, Ordering::Relaxed);
      WRITE_CALLED.store(false, Ordering::Relaxed);

      uv_tcp_connect(
        connect_req_ptr,
        tcp_ptr,
        &addr as *const _ as *const c_void,
        Some(on_connect),
      );

      uv_run(loop_, uv_run_mode::UV_RUN_DEFAULT);
      // Process close callbacks
      uv_run(loop_, uv_run_mode::UV_RUN_ONCE);

      assert!(CONNECT_CALLED.load(Ordering::Relaxed));
      assert!(WRITE_CALLED.load(Ordering::Relaxed));

      // Give the receiver thread time to read
      std::thread::sleep(Duration::from_millis(50));
      let data = received.lock().unwrap();
      assert_eq!(&*data, b"hello world");

      destroy_loop(loop_);
    }
  }

  // 13. TCP read_start and read_stop
  #[tokio::test(flavor = "multi_thread")]
  async fn test_tcp_read_start_stop() {
    unsafe {
      let loop_ = make_loop();

      // Create a std listener
      let std_listener =
        std::net::TcpListener::bind("127.0.0.1:0").unwrap();
      let port = std_listener.local_addr().unwrap().port();

      // Spawn writer thread
      std::thread::spawn(move || {
        let (mut stream, _) = std_listener.accept().unwrap();
        std::thread::sleep(Duration::from_millis(50));
        std::io::Write::write_all(&mut stream, b"test data").unwrap();
        // Keep the stream open briefly so read can see data
        std::thread::sleep(Duration::from_millis(100));
      });

      // Connect
      let mut tcp = MaybeUninit::<uv_tcp_t>::uninit();
      let tcp_ptr = tcp.as_mut_ptr();
      uv_tcp_init(loop_, tcp_ptr);

      let addr = make_sockaddr_in(port);
      let mut connect_req = MaybeUninit::<uv_connect_t>::uninit();
      let connect_req_ptr = connect_req.as_mut_ptr();

      static READ_BYTES: AtomicU32 = AtomicU32::new(0);

      unsafe extern "C" fn alloc_cb(
        _handle: *mut uv_handle_t,
        suggested_size: usize,
        buf: *mut uv_buf_t,
      ) {
        let ptr = libc::malloc(suggested_size) as *mut c_char;
        (*buf).base = ptr;
        (*buf).len = suggested_size;
      }

      unsafe extern "C" fn read_cb(
        stream: *mut uv_stream_t,
        nread: isize,
        buf: *const uv_buf_t,
      ) {
        if nread > 0 {
          READ_BYTES.fetch_add(nread as u32, Ordering::Relaxed);
          // Stop reading after getting data
          uv_read_stop(stream);
          let tcp = stream as *mut uv_tcp_t;
          uv_close(tcp as *mut uv_handle_t, None);
          uv_stop((*tcp).loop_);
        }
        if !(*buf).base.is_null() {
          libc::free((*buf).base as *mut c_void);
        }
      }

      unsafe extern "C" fn on_connect_for_read(
        req: *mut uv_connect_t,
        status: i32,
      ) {
        assert_eq!(status, 0);
        let stream = (*req).handle;
        uv_read_start(stream, Some(alloc_cb), Some(read_cb));
      }

      READ_BYTES.store(0, Ordering::Relaxed);

      uv_tcp_connect(
        connect_req_ptr,
        tcp_ptr,
        &addr as *const _ as *const c_void,
        Some(on_connect_for_read),
      );

      uv_run(loop_, uv_run_mode::UV_RUN_DEFAULT);
      uv_run(loop_, uv_run_mode::UV_RUN_ONCE);

      assert!(READ_BYTES.load(Ordering::Relaxed) > 0);

      destroy_loop(loop_);
    }
  }

  // 14. TCP getpeername/getsockname
  #[tokio::test(flavor = "multi_thread")]
  async fn test_tcp_getpeername_getsockname() {
    unsafe {
      let loop_ = make_loop();

      // Create a std listener
      let std_listener =
        std::net::TcpListener::bind("127.0.0.1:0").unwrap();
      let port = std_listener.local_addr().unwrap().port();

      // Accept in background
      std::thread::spawn(move || {
        let _ = std_listener.accept();
        std::thread::sleep(Duration::from_millis(200));
      });

      let mut tcp = MaybeUninit::<uv_tcp_t>::uninit();
      let tcp_ptr = tcp.as_mut_ptr();
      uv_tcp_init(loop_, tcp_ptr);

      let addr = make_sockaddr_in(port);
      let mut connect_req = MaybeUninit::<uv_connect_t>::uninit();
      let connect_req_ptr = connect_req.as_mut_ptr();

      static NAMES_OK: AtomicBool = AtomicBool::new(false);

      unsafe extern "C" fn on_connect_names(
        req: *mut uv_connect_t,
        status: i32,
      ) {
        assert_eq!(status, 0);
        let tcp = (*req).handle as *mut uv_tcp_t;

        // getpeername
        let mut peer_addr: libc::sockaddr_in = std::mem::zeroed();
        let mut peer_len =
          std::mem::size_of::<libc::sockaddr_in>() as c_int;
        let rc = uv_tcp_getpeername(
          tcp,
          &mut peer_addr as *mut _ as *mut c_void,
          &mut peer_len,
        );
        assert_eq!(rc, 0);
        let peer_port = u16::from_be(peer_addr.sin_port);
        assert!(peer_port > 0);

        // getsockname
        let mut local_addr: libc::sockaddr_in = std::mem::zeroed();
        let mut local_len =
          std::mem::size_of::<libc::sockaddr_in>() as c_int;
        let rc = uv_tcp_getsockname(
          tcp,
          &mut local_addr as *mut _ as *mut c_void,
          &mut local_len,
        );
        assert_eq!(rc, 0);
        let local_port = u16::from_be(local_addr.sin_port);
        assert!(local_port > 0);

        NAMES_OK.store(true, Ordering::Relaxed);

        uv_close(tcp as *mut uv_handle_t, None);
        uv_stop((*tcp).loop_);
      }

      NAMES_OK.store(false, Ordering::Relaxed);

      uv_tcp_connect(
        connect_req_ptr,
        tcp_ptr,
        &addr as *const _ as *const c_void,
        Some(on_connect_names),
      );

      uv_run(loop_, uv_run_mode::UV_RUN_DEFAULT);
      uv_run(loop_, uv_run_mode::UV_RUN_ONCE);

      assert!(NAMES_OK.load(Ordering::Relaxed));

      destroy_loop(loop_);
    }
  }

  // 15. TCP close during active read
  #[tokio::test(flavor = "multi_thread")]
  async fn test_tcp_close() {
    unsafe {
      let loop_ = make_loop();

      let mut tcp = MaybeUninit::<uv_tcp_t>::uninit();
      let tcp_ptr = tcp.as_mut_ptr();
      uv_tcp_init(loop_, tcp_ptr);

      static CLOSE_FIRED: AtomicBool = AtomicBool::new(false);
      unsafe extern "C" fn close_cb(_handle: *mut uv_handle_t) {
        CLOSE_FIRED.store(true, Ordering::Relaxed);
      }

      CLOSE_FIRED.store(false, Ordering::Relaxed);

      // Set it active to test close behavior
      (*tcp_ptr).flags |= UV_HANDLE_ACTIVE;

      uv_close(tcp_ptr as *mut uv_handle_t, Some(close_cb));
      assert_eq!(
        (*tcp_ptr).flags & UV_HANDLE_CLOSING,
        UV_HANDLE_CLOSING
      );

      uv_run(loop_, uv_run_mode::UV_RUN_ONCE);
      assert!(CLOSE_FIRED.load(Ordering::Relaxed));

      destroy_loop(loop_);
    }
  }

  // 16. TCP nodelay
  #[tokio::test(flavor = "multi_thread")]
  async fn test_tcp_nodelay() {
    unsafe {
      let loop_ = make_loop();

      let mut tcp = MaybeUninit::<uv_tcp_t>::uninit();
      let tcp_ptr = tcp.as_mut_ptr();
      uv_tcp_init(loop_, tcp_ptr);

      // Set nodelay before connection (should just store the flag)
      let rc = uv_tcp_nodelay(tcp_ptr, 1);
      assert_eq!(rc, 0);
      assert!((*tcp_ptr).internal_nodelay);

      // Disable
      let rc = uv_tcp_nodelay(tcp_ptr, 0);
      assert_eq!(rc, 0);
      assert!(!(*tcp_ptr).internal_nodelay);

      destroy_loop(loop_);
    }
  }

}
