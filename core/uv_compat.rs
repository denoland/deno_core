// Copyright 2018-2025 the Deno authors. MIT license.

use std::cell::Cell;
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::ffi::c_int;
use std::ffi::c_void;
use std::time::Duration;
use std::time::Instant;

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum uv_handle_type {
  UV_UNKNOWN_HANDLE = 0,
  UV_TIMER = 1,
  UV_IDLE = 2,
  UV_PREPARE = 3,
  UV_CHECK = 4,
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

pub type uv_timer_cb = unsafe extern "C" fn(*mut uv_timer_t);
pub type uv_idle_cb = unsafe extern "C" fn(*mut uv_idle_t);
pub type uv_prepare_cb = unsafe extern "C" fn(*mut uv_prepare_t);
pub type uv_check_cb = unsafe extern "C" fn(*mut uv_check_t);
pub type uv_close_cb = unsafe extern "C" fn(*mut uv_handle_t);

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

  closing_handles:
    RefCell<VecDeque<(*mut uv_handle_t, Option<uv_close_cb>)>>,

  time_origin: Instant,
}

impl UvLoopInner {
  fn new() -> Self {
    Self {
      timers: RefCell::new(BTreeSet::new()),
      next_timer_id: Cell::new(1),
      timer_handles: RefCell::new(HashMap::new()),
      idle_handles: RefCell::new(Vec::new()),
      prepare_handles: RefCell::new(Vec::new()),
      check_handles: RefCell::new(Vec::new()),
      closing_handles: RefCell::new(VecDeque::new()),
      time_origin: Instant::now(),
    }
  }

  fn alloc_timer_id(&self) -> u64 {
    let id = self.next_timer_id.get();
    self.next_timer_id.set(id + 1);
    id
  }

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
      self.run_idle();
      self.run_prepare();
      self.run_check();
      self.run_close();
    }
  }

  /// Returns true if there are any alive (active + ref'd) handles.
  fn has_alive_handles(&self) -> bool {
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
}

/// Get the `UvLoopInner` from a `uv_loop_t` pointer.
///
/// # Safety
/// `loop_` must be a valid, initialized loop pointer.
#[inline]
unsafe fn get_inner(loop_: *mut uv_loop_t) -> &'static UvLoopInner {
  unsafe { &*((*loop_).internal as *const UvLoopInner) }
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
          // Sleep until next timer if no immediate work
          if inner.idle_handles.borrow().is_empty()
            && inner.closing_handles.borrow().is_empty()
          {
            if let Some(deadline) = inner.next_timer_deadline_ms() {
              let now = inner.now_ms();
              if deadline > now {
                std::thread::sleep(Duration::from_millis(deadline - now));
              }
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
        // Sleep until next timer deadline
        if let Some(deadline) = inner.next_timer_deadline_ms() {
          let now = inner.now_ms();
          if deadline > now
            && inner.idle_handles.borrow().is_empty()
            && inner.closing_handles.borrow().is_empty()
          {
            std::thread::sleep(Duration::from_millis(deadline - now));
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
}
