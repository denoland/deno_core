// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use cooked_waker::IntoWaker;
use cooked_waker::ViaRawPointer;
use cooked_waker::Wake;
use cooked_waker::WakeRef;
use futures::Future;
use std::cell::Cell;
use std::cell::RefCell;
use std::cell::UnsafeCell;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::mem::MaybeUninit;
use std::num::NonZeroU64;
use std::pin::Pin;
use std::task::ready;
use std::task::Context;
use std::task::Poll;
use std::task::Waker;
use std::time::Duration;
use tokio::time::Instant;
use tokio::time::Sleep;

pub(crate) type WebTimerId = u64;

#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum TimerType {
  Repeat(NonZeroU64),
  Once,
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct TimerKey(Instant, u64, TimerType);

/// Implements much of the specification described by https://html.spec.whatwg.org/multipage/timers-and-user-prompts.html.
///
/// To ensure that we perform well in the face of large numbers of timers, this implementation assumes
/// that timers may complete in batches that are properly ordered according to the spec. That is to say, timers
/// are executed in an order according to the following specification:
///
/// > Wait until any invocations of this algorithm that had the same global and orderingIdentifier,
/// > that started before this one, and whose milliseconds is equal to or less than this one's,
/// > have completed.
///
/// This complicates timer resolution because timers fire in an order based on _milliseconds_ (aka their original
/// timeout duration) rather than an exact moment in time.
///
/// While we respect the spirit of this paragraph, we make an assumption that all timer callback invocations are
/// instantaneous. This means that we can assume that no further timers have elapsed during the execution of a batch of
/// timers. This does not always hold up in reality (for example, a poorly-written timer in a worker may block on
/// `Atomics.wait` or a synchronous `XMLHttpRequest`), but it allows us to process timers in a simpler and less-racy way.
///
/// We also assume that our underlying timers are high-resolution, and that our underlying timer source resolves in
/// a proper start time + expiration time order.
///
/// Finally, we assume that the event loop time -- the time between which we are able to poll this set of timers -- is
/// non-zero, and that multiple timers may fire during this period and require re-ordering by their original millisecond
/// timeouts.
///
///
/// https://github.com/denoland/deno/pull/12953 -- Add refTimer, unrefTimer API
/// https://github.com/denoland/deno/pull/12862 -- Refactor timers to use one async op per timer
///
/// https://github.com/denoland/deno/issues/11398 -- Spurious assertion error when the callback to setInterval lasts longer than the interval
pub(crate) struct WebTimers<T> {
  next_id: Cell<WebTimerId>,
  timers: RefCell<BTreeSet<TimerKey>>,
  /// We choose a `BTreeMap` over `HashMap` because of memory performance.
  data_map: RefCell<BTreeMap<WebTimerId, (T, bool)>>,
  /// How may deleted entries are in the timers BTreeMap? We use this to determine
  /// when there's too much garbage and need to rebuild the map.
  tombstone_count: Cell<usize>,
  /// How many unref'd timers exist?
  unrefd_count: Cell<usize>,
  /// A boxed MutableSleep that will allow us to change the Tokio sleep timeout as needed.
  /// Because this is boxed, we can "safely" unsafely poll it.
  sleep: Box<MutableSleep>,
}

impl<T> Default for WebTimers<T> {
  fn default() -> Self {
    Self {
      next_id: Default::default(),
      timers: Default::default(),
      data_map: Default::default(),
      tombstone_count: Default::default(),
      unrefd_count: Default::default(),
      sleep: MutableSleep::new(),
    }
  }
}

struct MutableSleep {
  sleep: UnsafeCell<Option<Sleep>>,
  ready: Cell<bool>,
  external_waker: UnsafeCell<Option<Waker>>,
  internal_waker: Waker,
}

#[allow(clippy::borrowed_box)]
impl MutableSleep {
  fn new() -> Box<Self> {
    let mut new = Box::new(MaybeUninit::<Self>::uninit());

    new.write(MutableSleep {
      sleep: Default::default(),
      ready: Cell::default(),
      external_waker: UnsafeCell::default(),
      internal_waker: MutableSleepWaker {
        inner: new.as_ptr(),
      }
      .into_waker(),
    });
    unsafe { std::mem::transmute(new) }
  }

  fn poll_ready(self: &Box<Self>, cx: &mut Context) -> Poll<()> {
    if self.ready.take() {
      Poll::Ready(())
    } else {
      let external =
        unsafe { self.external_waker.get().as_mut().unwrap_unchecked() };
      if let Some(external) = external {
        // Already have this waker
        let waker = cx.waker();
        if !external.will_wake(waker) {
          *external = waker.clone();
        }
        Poll::Pending
      } else {
        *external = Some(cx.waker().clone());
        Poll::Pending
      }
    }
  }

  fn clear(self: &Box<Self>) {
    unsafe {
      *self.sleep.get() = None;
    }
  }

  fn change(self: &Box<Self>, instant: Instant) {
    let pin = unsafe {
      // First replace the current sleep
      *self.sleep.get() = Some(tokio::time::sleep_until(instant));

      // Then get ourselves a Pin to this
      Pin::new_unchecked(
        self
          .sleep
          .get()
          .as_mut()
          .unwrap_unchecked()
          .as_mut()
          .unwrap_unchecked(),
      )
    };

    // Register our waker
    let waker = &self.internal_waker;
    if pin.poll(&mut Context::from_waker(waker)).is_ready() {
      self.ready.set(true);
      self.internal_waker.wake_by_ref();
    }
  }
}

#[repr(transparent)]
#[derive(Clone)]
struct MutableSleepWaker {
  inner: *const MutableSleep,
}

unsafe impl Send for MutableSleepWaker {}
unsafe impl Sync for MutableSleepWaker {}

impl WakeRef for MutableSleepWaker {
  fn wake_by_ref(&self) {
    unsafe {
      let this = self.inner.as_ref().unwrap_unchecked();
      this.ready.set(true);
      let waker = this.external_waker.get().as_mut().unwrap_unchecked();
      if let Some(waker) = waker.as_ref() {
        waker.wake_by_ref();
      }
    }
  }
}

impl Wake for MutableSleepWaker {
  fn wake(self) {
    self.wake_by_ref()
  }
}

impl Drop for MutableSleepWaker {
  fn drop(&mut self) {}
}

unsafe impl ViaRawPointer for MutableSleepWaker {
  type Target = ();

  fn into_raw(self) -> *mut () {
    self.inner as _
  }

  unsafe fn from_raw(ptr: *mut ()) -> Self {
    MutableSleepWaker { inner: ptr as _ }
  }
}

impl<T: Clone> WebTimers<T> {
  /// Refs a timer by ID. Invalid IDs are ignored.
  pub fn ref_timer(&self, id: WebTimerId) {
    if let Some((_, unrefd)) = self.data_map.borrow_mut().get_mut(&id) {
      if std::mem::replace(unrefd, false) {
        self.unrefd_count.set(self.unrefd_count.get() - 1);
      }
    }
  }

  /// Unrefs a timer by ID. Invalid IDs are ignored.
  pub fn unref_timer(&self, id: WebTimerId) {
    if let Some((_, unrefd)) = self.data_map.borrow_mut().get_mut(&id) {
      if !std::mem::replace(unrefd, true) {
        self.unrefd_count.set(self.unrefd_count.get() + 1);
      }
    }
  }

  /// Queues a timer to be fired in order with the other timers in this set of timers.
  pub fn queue_timer(&self, timeout_ms: u64, data: T) -> WebTimerId {
    self.queue_timer_internal(false, timeout_ms, data)
  }

  /// Queues a timer to be fired in order with the other timers in this set of timers.
  pub fn queue_timer_repeat(&self, timeout_ms: u64, data: T) -> WebTimerId {
    self.queue_timer_internal(true, timeout_ms, data)
  }

  fn queue_timer_internal(
    &self,
    repeat: bool,
    timeout_ms: u64,
    data: T,
  ) -> WebTimerId {
    let id = self.next_id.get() + 1;
    self.next_id.set(id);

    let mut timers = self.timers.borrow_mut();
    let deadline = Instant::now()
      .checked_add(Duration::from_millis(timeout_ms))
      .unwrap();
    if let Some(TimerKey(k, ..)) = timers.first() {
      if &deadline < k {
        self.sleep.change(deadline);
      }
    } else {
      self.sleep.change(deadline);
    }

    let timer_type = if repeat {
      TimerType::Repeat(
        NonZeroU64::new(timeout_ms).unwrap_or(NonZeroU64::new(1).unwrap()),
      )
    } else {
      TimerType::Once
    };
    timers.insert(TimerKey(deadline, id, timer_type));

    let mut data_map = self.data_map.borrow_mut();
    data_map.insert(id, (data, false));
    id
  }

  /// Cancels a pending timer in this set of timers, returning the associated data if a timer
  /// with the given ID was found.
  pub fn cancel_timer(&self, timer: u64) -> Option<T> {
    let mut data_map = self.data_map.borrow_mut();
    if let Some((data, unrefd)) = data_map.remove(&timer) {
      if data_map.is_empty() {
        // When the # of running timers hits zero, clear the timer tree and
        // tombstone count.
        self.unrefd_count.set(0);
        self.timers.borrow_mut().clear();
        self.tombstone_count.set(0);
        self.sleep.clear();
      } else {
        if unrefd {
          self.unrefd_count.set(self.unrefd_count.get() - 1);
        }

        self.tombstone_count.set(self.tombstone_count.get() + 1);
      }
      Some(data)
    } else {
      None
    }
  }

  /// Poll for any timers that have completed.
  pub fn poll_timers(&self, cx: &mut Context) -> Poll<Vec<(u64, T)>> {
    ready!(self.sleep.poll_ready(cx));
    let now = Instant::now();
    let mut timers = self.timers.borrow_mut();
    let mut data = self.data_map.borrow_mut();
    let mut output = vec![];

    let mut split = timers.split_off(&TimerKey(now, 0, TimerType::Once));
    std::mem::swap(&mut split, &mut timers);
    for TimerKey(_, id, timer_type) in split {
      if let TimerType::Repeat(interval) = timer_type {
        if let Some((data, _)) = data.get(&id) {
          output.push((id, data.clone()));
          timers.insert(TimerKey(
            now
              .checked_add(Duration::from_millis(interval.into()))
              .unwrap(),
            id,
            timer_type,
          ));
        } else {
          self.tombstone_count.set(self.tombstone_count.get() - 1);
        }
      } else if let Some((data, unrefd)) = data.remove(&id) {
        if unrefd {
          self.unrefd_count.set(self.unrefd_count.get() - 1);
        }
        output.push((id, data));
      } else {
        self.tombstone_count.set(self.tombstone_count.get() - 1);
      }
    }

    // In-effective poll, run a front-compaction and try again later
    if output.is_empty() {
      // We should never have an ineffective poll when the data map is empty, as we check
      // for this in cancel_timer.
      debug_assert!(!data.is_empty());
      while let Some(TimerKey(_, id, _)) = timers.first() {
        if data.contains_key(id) {
          break;
        } else {
          timers.pop_first();
        }
      }
      if let Some(TimerKey(k, ..)) = timers.first() {
        self.sleep.change(*k);
      }
      return Poll::Pending;
    }

    if data.is_empty() {
      // When the # of running timers hits zero, clear the timer tree and
      // tombstone count.
      if self.tombstone_count.take() > 0 {
        timers.clear();
        self.sleep.clear();
      }
    } else {
      const COMPACTION_MINIMUM: usize = 16;
      // If we have more tombstones than data, and tombstones are > COMPACTION_MINIMUM, run a compaction
      if self.tombstone_count.get() > data.len()
        && self.tombstone_count.get() > COMPACTION_MINIMUM
      {
        self.tombstone_count.set(0);
        timers.retain(|k| data.contains_key(&k.1));
      }
      if let Some(TimerKey(k, ..)) = timers.first() {
        self.sleep.change(*k);
      }
    }

    Poll::Ready(output)
  }

  /// Is this set of timers empty?
  #[cfg(test)]
  pub fn is_empty(&self) -> bool {
    self.data_map.borrow().is_empty()
  }

  #[cfg(test)]
  pub fn len(&self) -> usize {
    self.data_map.borrow().len()
  }

  #[cfg(test)]
  pub fn assert_consistent(&self) {
    if self.data_map.borrow().is_empty() {
      // If the data map is empty, we should have no timers, no tombstones and no unref'd count
      assert_eq!(self.timers.borrow().len(), 0);
      assert_eq!(self.tombstone_count.get(), 0);
      assert_eq!(self.unrefd_count.get(), 0);
    } else {
      assert_eq!(
        self.timers.borrow().len(),
        self.data_map.borrow().len() + self.tombstone_count.get()
      );
      assert!(self.unrefd_count.get() <= self.data_map.borrow().len());
    }
  }

  pub fn has_pending_timers(&self) -> bool {
    self.timers.borrow().len() > self.unrefd_count.get()
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use futures::future::poll_fn;
  use rstest::rstest;

  /// Miri is way too slow here on some of the larger tests.
  const TEN_THOUSAND: u64 = if cfg!(miri) { 100 } else { 10_000 };

  /// Helper function to support miri + rstest. We cannot use I/O in a miri test.
  fn async_test<F: Future<Output = T>, T>(f: F) -> T {
    let runtime = tokio::runtime::Builder::new_current_thread()
      .enable_time()
      .build()
      .unwrap();
    runtime.block_on(f)
  }

  async fn poll_all(timers: &WebTimers<()>) -> Vec<(u64, ())> {
    timers.assert_consistent();
    let len = timers.len();
    let mut v = vec![];
    while !timers.is_empty() {
      let mut batch = poll_fn(|cx| timers.poll_timers(cx)).await;
      v.append(&mut batch);
      eprintln!("{}", v.len());
      timers.assert_consistent();
    }
    assert_eq!(v.len(), len);
    v
  }

  #[test]
  fn test_timer() {
    async_test(async {
      let timers = WebTimers::<()>::default();
      let _a = timers.queue_timer(1, ());

      let v = poll_all(&timers).await;
      assert_eq!(v.len(), 1);
    });
  }

  #[rstest]
  #[test]
  fn test_timer_cancel_1(#[values(0, 1)] which: u64) {
    async_test(async {
      let timers = WebTimers::<()>::default();
      for i in 0..2 {
        let id = timers.queue_timer(i * 100, ());
        if i == which {
          assert!(timers.cancel_timer(id).is_some());
        }
      }
      assert_eq!(timers.len(), 1);

      let v = poll_all(&timers).await;
      assert_eq!(v.len(), 1);
    })
  }

  #[test]
  fn test_timers_10_random() {
    async_test(async {
      let timers = WebTimers::<()>::default();
      for i in 0..10 {
        timers.queue_timer((i % 3) * 10, ());
      }

      let v = poll_all(&timers).await;
      assert_eq!(v.len(), 10);
    })
  }

  #[test]
  fn test_timers_10_random_cancel() {
    async_test(async {
      let timers = WebTimers::<()>::default();
      for i in 0..10 {
        let id = timers.queue_timer((i % 3) * 10, ());
        timers.cancel_timer(id);
      }

      let v = poll_all(&timers).await;
      assert_eq!(v.len(), 0);
    });
  }

  #[rstest]
  #[test]
  fn test_timers_10_random_cancel_after(#[values(true, false)] reverse: bool) {
    async_test(async {
      let timers = WebTimers::<()>::default();
      let mut ids = vec![];
      for i in 0..2 {
        ids.push(timers.queue_timer((i % 3) * 10, ()));
      }
      if reverse {
        ids.reverse();
      }
      for id in ids {
        timers.cancel_timer(id);
        timers.assert_consistent();
      }

      let v = poll_all(&timers).await;
      assert_eq!(v.len(), 0);
    });
  }

  #[test]
  fn test_timers_10() {
    async_test(async {
      let timers = WebTimers::<()>::default();
      for _i in 0..10 {
        timers.queue_timer(1, ());
      }

      let v = poll_all(&timers).await;
      assert_eq!(v.len(), 10);
    });
  }

  #[test]
  fn test_timers_10_000_random() {
    async_test(async {
      let timers = WebTimers::<()>::default();
      for i in 0..TEN_THOUSAND {
        timers.queue_timer(i % 10, ());
      }

      let v = poll_all(&timers).await;
      assert_eq!(v.len(), TEN_THOUSAND as usize);
    });
  }

  /// Cancel a large number of timers at the head of the queue to trigger
  /// a front compaction.
  #[test]
  fn test_timers_cancel_first() {
    async_test(async {
      let timers = WebTimers::<()>::default();
      let mut ids = vec![];
      for _ in 0..TEN_THOUSAND {
        ids.push(timers.queue_timer(1, ()));
      }
      for _ in 0..10 {
        timers.queue_timer(1000, ());
      }
      for id in ids {
        timers.cancel_timer(id);
      }
      let v = poll_all(&timers).await;
      assert_eq!(v.len(), 10);
    });
  }

  #[test]
  fn test_timers_10_000_cancel_most() {
    async_test(async {
      let timers = WebTimers::<()>::default();
      let mut ids = vec![];
      for i in 0..TEN_THOUSAND {
        ids.push(timers.queue_timer(i % 100, ()));
      }

      // This should trigger a compaction
      fastrand::seed(42);
      ids.retain(|_| fastrand::u8(0..10) > 0);
      for id in ids.iter() {
        timers.cancel_timer(*id);
      }

      let v = poll_all(&timers).await;
      assert_eq!(v.len(), TEN_THOUSAND as usize - ids.len());
    });
  }

  #[rstest]
  #[test]
  fn test_chaos(#[values(42, 99, 1000)] seed: u64) {
    async_test(async {
      let timers = WebTimers::<()>::default();
      fastrand::seed(seed);

      let mut count = 0;
      let mut ref_count = 0;

      for _ in 0..TEN_THOUSAND {
        let mut cancelled = false;
        let mut unrefed = false;
        let id = timers.queue_timer(fastrand::u64(0..10), ());
        for _ in 0..fastrand::u64(0..10) {
          if fastrand::u8(0..10) == 0 {
            timers.cancel_timer(id);
            cancelled = true;
          }
          if fastrand::u8(0..10) == 0 {
            timers.ref_timer(id);
            unrefed = false;
          }
          if fastrand::u8(0..10) == 0 {
            timers.unref_timer(id);
            unrefed = true;
          }
        }

        if !cancelled {
          count += 1;
        }
        if !unrefed {
          ref_count += 1;
        }

        timers.assert_consistent();
      }

      eprintln!("count={count} ref_count={ref_count}");

      let v = poll_all(&timers).await;
      assert_eq!(v.len(), count);

      assert!(timers.is_empty());
      assert!(!timers.has_pending_timers());
    });
  }
}
