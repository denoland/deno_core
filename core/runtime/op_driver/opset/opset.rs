use futures::task::noop_waker;
use futures::task::AtomicWaker;
use futures::Future;
use std::alloc::handle_alloc_error;
use std::alloc::Layout;
use std::cell::UnsafeCell;
use std::future::poll_fn;
use std::marker::PhantomData;
use std::mem::MaybeUninit;
use std::pin::Pin;
use std::sync::atomic::AtomicU32;
use std::sync::atomic::Ordering;
use std::task::Context;
use std::task::Poll;
use std::task::RawWaker;
use std::task::RawWakerVTable;
use std::task::Waker;

use crate::arena::ArenaArc;
use crate::arena::ArenaSharedAtomic;

use super::lock_free_queue::IntrusiveQueue;
use super::lock_free_queue::IntrusiveQueueNodeStorage;
use super::lock_free_queue::IntrusiveQueueStorage;
use super::pointers::Ptr;

/// The maximum size of a future we will support.
const FUTURE_SIZE: usize = 256;
/// The base capacity of future we support. Our memory requirements for this storage
/// are `BASE_CAPACITY` * `FUTURE_SIZE`.
const BASE_CAPACITY: usize = 1024 * 100;

/// Maintains a list of ops, akin to a [`JoinSet`] but removes the large allocation
/// requirement.
///
/// The complication with ops is that -- for the most part -- the futures cannot be moved and
/// must stay pinned in memory until the time they are completed. In addition, the waker
/// we pass to op functions _may_ outlive this [`OpSet`].
///
///
pub struct OpSet<C, O> {
  /// The inner parts of the OpSet that cannot move.
  inner: *mut OpSetInner<Types<C, O>>,

  _output: PhantomData<O>,
  _context: PhantomData<C>,
  _unsend_marker: PhantomData<*const ()>,
}

impl<C, O> Drop for OpSet<C, O> {
  fn drop(&mut self) {
    // for i in 0..BASE_CAPACITY {
    //   let entry = unsafe { self.inner.base_capacity.add(i).as_ref().unwrap_unchecked() };
    //   if let FutureEntryItem::Occupied(entry) = entry {
    //     unsafe { (entry.assume_init_ref().drop_fn)(entry.assume_init_ref()) }
    //   }
    // }
    // eprintln!("opset drop");
    unsafe { OpSetInner::notify_waker_dropped(self.inner) };
  }
}

trait OpSetAssociatedTypes {
  type Context;
  type Output;
}

struct Types<C, O>(PhantomData<(C, O)>);

impl<C, O> OpSetAssociatedTypes for Types<C, O> {
  type Context = C;
  type Output = O;
}

struct OpSetInner<T: OpSetAssociatedTypes> {
  /// The base capacity for futures. We will always maintain the memory for these futures.
  arena: ArenaSharedAtomic<FutureEntry<T>, BASE_CAPACITY>,
  /// As wakers may outlive this OpSet in degenerate cases, we keep a refcount to ensure that
  /// we don't drop until the long-lived wakers have gone away.
  refcount: AtomicU32,
  /// The outer waker that has polled this [`OpSet`].
  outer_waker: AtomicWaker,

  queue: IntrusiveQueue<FutureEntry<T>>,
}

impl<T: OpSetAssociatedTypes> OpSetInner<T> {
  unsafe fn alloc() -> *mut Self {
    let layout = Layout::new::<Self>();
    let ptr = std::alloc::alloc(layout) as *mut Self;
    if ptr.is_null() {
      handle_alloc_error(layout);
    }
    std::ptr::write(
      ptr,
      Self {
        arena: Default::default(),
        refcount: AtomicU32::new(1),
        outer_waker: Default::default(),
        queue: Default::default(),
      },
    );
    ptr
  }

  unsafe fn dealloc(this: *mut Self) {
    let layout = Layout::new::<Self>();
    std::alloc::dealloc(this as _, layout);
  }

  /// Notify us that one of our wakers has woken. This call may come in from any thread.
  #[inline(always)]
  fn notify_from_waker(&self, entry: *mut FutureEntry<T>) {
    unsafe { IntrusiveQueue::push(&self.queue, Ptr::from(entry)) };
    self.outer_waker.wake();
  }

  unsafe fn notify_waker_dropped(this: *mut Self) {
    if (*this).refcount.fetch_sub(1, Ordering::AcqRel) == 1 {
      // eprintln!("dealloc");
      std::ptr::drop_in_place(this);
      unsafe { std::alloc::dealloc(this as _, Layout::new::<Self>()) }
    }
  }
}

static_assertions::assert_not_impl_all!(OpSet<(), ()>: Send);
static_assertions::assert_not_impl_all!(OpSet<(), ()>: Sync);

impl<C, O> Default for OpSet<C, O> {
  fn default() -> Self {
    let this = Self {
      inner: unsafe { OpSetInner::<Types<C, O>>::alloc() },
      _output: PhantomData,
      _context: PhantomData,
      _unsend_marker: PhantomData,
    };

    this
  }
}

#[repr(align(16))]
struct AlignU128<T>(T);

/// Holds the future and space for the result, in case we need to poll this future without
/// immediately returning the result.
struct FutureAndResultLayout<F> {
  future: F,
}

struct FutureEntry<T: OpSetAssociatedTypes> {
  memory: AlignU128<UnsafeCell<[MaybeUninit<u8>; FUTURE_SIZE]>>,
  data: FutureEntryData<T>,
}

impl<T: OpSetAssociatedTypes> Ptr<FutureEntry<T>> {
  fn data(&self) -> &FutureEntryData<T> {
    let ptr = self.into_raw();
    let data = unsafe { std::ptr::addr_of!((*ptr).data) };
    unsafe { &*data }
  }

  fn ptr(&self) -> *mut () {
    let ptr = self.into_raw();
    unsafe { std::ptr::addr_of!((*ptr).memory) as *mut () }
  }
}

impl<T: OpSetAssociatedTypes> IntrusiveQueueStorage<FutureEntry<T>>
  for FutureEntry<T>
{
  #[inline(always)]
  unsafe fn queue_node(
    node: *mut FutureEntry<T>,
  ) -> *mut IntrusiveQueueNodeStorage<FutureEntry<T>> {
    std::ptr::addr_of_mut!((*node).data.queue_node)
  }
}

type MapFn<C, O, R> = fn(&mut C, R) -> O;
type ErasedMapFn<C, O> = unsafe fn(
  cx: &mut Context,
  ctx: &mut C,
  map_fn: *mut (),
  ptr: *mut (),
) -> Poll<O>;

struct FutureEntryData<T: OpSetAssociatedTypes> {
  id: u32,

  map_fn: *mut (),
  poll_map_fn: ErasedMapFn<T::Context, T::Output>,

  dedicated_waker: UnsafeCell<Waker>,

  container: *const OpSetInner<T>,

  /// Used for the intrusive queue's per-node storage.
  queue_node: IntrusiveQueueNodeStorage<FutureEntry<T>>,
}

impl<T: OpSetAssociatedTypes> FutureEntryData<T> {
  unsafe fn poll_map<F, R>(
    cx: &mut Context,
    ctx: &mut T::Context,
    map_fn: *mut (),
    ptr: *mut (),
  ) -> Poll<T::Output>
  where
    F: Future<Output = R>,
  {
    let ptr = ptr as *mut FutureAndResultLayout<F>;
    let future_and_result = &mut *ptr;

    let res = Pin::new_unchecked(&mut future_and_result.future).poll(cx);
    if let Poll::Ready(ready) = res {
      let actual_map_fn: MapFn<T::Context, T::Output, R> =
        std::mem::transmute(map_fn);
      let res = (actual_map_fn)(ctx, ready);
      std::ptr::drop_in_place(ptr);
      Poll::Ready(res)
    } else {
      Poll::Pending
    }
  }

  unsafe fn dedicated_waker_clone(data: *const ()) -> RawWaker {
    let entry = data as *mut FutureEntry<T>;
    ArenaArc::clone_raw_from_raw(entry);
    RawWaker::new(entry as _, &Self::waker_vtable())
  }

  unsafe fn dedicated_waker_wake(data: *const ()) {
    let entry = data as *mut FutureEntry<T>;
    let data = std::ptr::addr_of!((*entry).data);
    (*(*data).container).notify_from_waker(entry);
    ArenaArc::drop_from_raw(entry);
  }

  unsafe fn dedicated_waker_wake_by_ref(data: *const ()) {
    let entry = data as *mut FutureEntry<T>;
    let data = std::ptr::addr_of!((*entry).data);
    (*(*data).container).notify_from_waker(entry);
  }

  unsafe fn dedicated_waker_drop(data: *const ()) {
    let entry = data as *mut FutureEntry<T>;
    ArenaArc::drop_from_raw(entry);
  }

  const fn waker_vtable() -> &'static RawWakerVTable {
    &RawWakerVTable::new(
      Self::dedicated_waker_clone,
      Self::dedicated_waker_wake,
      Self::dedicated_waker_wake_by_ref,
      Self::dedicated_waker_drop,
    )
  }
}

trait InsertOp<C, O, F, R>
where
  F: Future<Output = R>,
{
  const SIZE_OK: ();
  const ALIGN_OK: ();

  /// Insert an op that returns T, along with a mapping function that can poll the future
  /// along with a context object and return O if it is ready.
  ///
  /// The [`Future`] is polled upon insertion. If the future is ready immediately (ie: returns
  /// [`Poll::Ready`]), this function returns the output `T` and does not insert the op
  /// into the [`OpSet`].
  fn insert_op(&mut self, id: u32, f: F, poll_fn: MapFn<C, O, R>) -> Poll<R>;
}

impl<C, O, F, R> InsertOp<C, O, F, R> for OpSet<C, O>
where
  F: Future<Output = R>,
{
  const SIZE_OK: () = assert!(std::mem::size_of::<F>() <= FUTURE_SIZE);
  const ALIGN_OK: () =
    assert!(std::mem::align_of::<F>() <= std::mem::align_of::<AlignU128<()>>());

  /// Insert an op future and the function required to map it to the output type.
  fn insert_op(&mut self, id: u32, f: F, r: MapFn<C, O, R>) -> Poll<R> {
    let _ = <Self as InsertOp<C, O, F, R>>::SIZE_OK;
    let _ = <Self as InsertOp<C, O, F, R>>::ALIGN_OK;

    unsafe { self.insert_op_inner(id, f, r) }
  }
}

impl<C, O> OpSet<C, O> {
  pub fn insert_op<F, R>(&mut self, id: u32, f: F, r: MapFn<C, O, R>) -> Poll<R>
  where
    F: Future<Output = R>,
  {
    <Self as InsertOp<C, O, F, R>>::insert_op(self, id, f, r)
  }

  pub fn insert_op_unmapped<F>(&mut self, id: u32, f: F) -> Poll<O>
  where
    F: Future<Output = O>,
  {
    <Self as InsertOp<C, O, F, O>>::insert_op(self, id, f, |_, x| x)
  }

  pub fn poll_ready(
    &mut self,
    cx: &mut Context,
    ctx: &mut C,
  ) -> Poll<(u32, O)> {
    unsafe {
      let queue = &(*self.inner).queue as *const _;
      loop {
        let first = IntrusiveQueue::pop(queue);
        if first.is_null() {
          break;
        }
        // unsafe { eprintln!("{} poll", (*first).data.id); }
        let ptr = first.ptr();
        let data = first.data();
        let map_fn = data.map_fn;

        if let Poll::Ready(v) = (data.poll_map_fn)(cx, ctx, map_fn, ptr) {
          return Poll::Ready((0, v));
        } else {
          // Not complete, let it return to pool
        }
      }
    }

    unsafe {
      (*self.inner).outer_waker.register(cx.waker());
    }
    Poll::Pending
  }

  unsafe fn insert_op_inner<F, R>(
    &self,
    id: u32,
    f: F,
    r: MapFn<C, O, R>,
  ) -> Poll<R>
  where
    F: Future<Output = R>,
  {
    // Re-check at runtime
    debug_assert!(std::mem::size_of::<F>() <= FUTURE_SIZE);
    debug_assert!(
      std::mem::align_of::<F>() <= std::mem::align_of::<AlignU128<()>>()
    );
    // Sanity check that we can safely transmute between these two types (this is guaranteed by
    // the Pin type and Rust's underlying pointer types but we demonstrate it here). This check
    // should be elided by the compiler as it is always true.
    debug_assert_eq!(
      std::mem::size_of::<Pin<&mut F>>(),
      std::mem::size_of::<Pin<*mut ()>>()
    );
    debug_assert_eq!(
      std::mem::align_of::<Pin<&mut F>>(),
      std::mem::align_of::<Pin<*mut ()>>()
    );

    let inner = unsafe { self.inner.as_ref().unwrap() };
    inner.refcount.fetch_add(1, Ordering::AcqRel);

    // Move the future to the buffer and forget about it
    let memory =
      AlignU128(UnsafeCell::new([MaybeUninit::uninit(); FUTURE_SIZE]));
    let f = FutureAndResultLayout { future: f };
    std::ptr::copy_nonoverlapping(&f, memory.0.get() as _, 1);
    std::mem::forget(f);

    let entry = inner.arena.allocate(FutureEntry {
      memory,
      data: FutureEntryData {
        id,
        poll_map_fn: FutureEntryData::<Types<C, O>>::poll_map::<F, R>,
        map_fn: r as _,
        dedicated_waker: UnsafeCell::new(noop_waker()),
        container: self.inner,
        queue_node: Default::default(),
      },
    });

    let raw = ArenaArc::into_raw(entry);
    let waker = Waker::from_raw(RawWaker::new(
      raw as _,
      FutureEntryData::<Types<C, O>>::waker_vtable(),
    ));

    let ptr = (*raw).memory.0.get() as *mut FutureAndResultLayout<F>;
    let f = Pin::new_unchecked(&mut (*ptr).future);
    let res = f.poll(&mut Context::from_waker(&waker));
    if res.is_pending() {
      // Pending, so save the dedicated waker
      *(*raw).data.dedicated_waker.get() = waker;
    }
    res
  }

  pub fn reset(&mut self) {
    unsafe {
      (*self.inner).outer_waker.take();
    }
  }

  pub async fn ready(&mut self, ctx: &mut C) -> (u32, O) {
    poll_fn(|cx| self.poll_ready(cx, ctx)).await
  }
}

#[cfg(test)]
mod tests {
  use rstest::rstest;
  use tokio::net::TcpListener;

  use super::*;
  use std::net::Ipv4Addr;
  use std::net::SocketAddr;
  use std::net::SocketAddrV4;

  /// Helper function to support miri + rstest. We cannot use I/O in a miri test.
  fn async_test<F: Future<Output = T>, T>(f: F) -> T {
    let runtime = tokio::runtime::Builder::new_current_thread()
      .enable_time()
      .build()
      .unwrap();
    runtime.block_on(f)
  }

  async fn task_ready() -> u32 {
    1
  }

  async fn task_not_ready() -> u32 {
    tokio::task::yield_now().await;
    // _ = File::open("/tmp/x").await;
    _ = TcpListener::bind(SocketAddr::V4(SocketAddrV4::new(
      Ipv4Addr::LOCALHOST,
      0,
    )));
    1
  }

  fn map_identity<T>(ctx: &mut (), from: T) -> T {
    from
  }

  async fn await_n<O>(opset: &mut OpSet<(), O>, count: usize) {
    for _ in 0..count {
      opset.ready(&mut ()).await;
    }
  }

  #[test]
  fn test_ready() {
    async_test(async {
      let mut opset: OpSet<(), u32> = OpSet::default();
      assert_eq!(opset.insert_op_unmapped(0, task_ready()), Poll::Ready(1));
    });
  }

  #[test]
  fn test_not_ready_single() {
    async_test(async {
      let mut opset: OpSet<(), u32> = OpSet::default();
      assert_eq!(opset.insert_op_unmapped(0, task_not_ready()), Poll::Pending);
      await_n(&mut opset, 1).await;
    });
  }

  #[test]
  fn test_not_ready_10() {
    async_test(async {
      let mut opset: OpSet<(), u32> = OpSet::default();
      for _ in 0..100 {
        eprintln!("x");
        for i in 0..10 {
          assert_eq!(
            opset.insert_op_unmapped(i, task_not_ready()),
            Poll::Pending
          );
        }
        await_n(&mut opset, 10).await;
        eprintln!("y");
      }
    });
  }

  // x
  // 1 wake: 2
  // 0 wake: 2
  // 0 wakeref 2
  // 1 wakeref 2
  // 0 drop 2
  // 0 drop 1
  // 1 drop 2
  // 1 drop 1

  // #[rstest]
  // #[test]
  // fn test_multiple(
  //   #[values(1, 2)] loops: usize,
  //   #[values(1, 2)] counts: usize,
  // ) {
  //   async_test(async {
  //     let mut opset: OpSet<(), u32> = OpSet::default();
  //     for i in 0..loops {
  //       for j in 0..counts {
  //         assert_eq!(
  //           opset.insert_op(
  //             (i * counts + j) as _,
  //             task_not_ready(),
  //             map_identity::<u32>
  //           ),
  //           None
  //         );
  //       }
  //       for _ in 0..counts {
  //         opset.next(&mut ()).await;
  //       }
  //     }
  //   });
  // }

  // #[test]
  // fn test_file() {
  //   async_test(async {
  //     let mut opset: OpSet<(), String> = OpSet::default();
  //     for i in 0..2 {
  //       for j in 0..2 {
  //         assert_eq!(
  //           opset.insert_op(
  //             i * 2 + j,
  //             async {
  //               tokio::task::yield_now().await;
  //               _ = File::open("/tmp/f").await;
  //               1_u32
  //             },
  //             |cx, scope, f| {
  //               let v = ready!(f.poll(cx));
  //               Poll::Ready(v.to_string())
  //             }
  //           ),
  //           None
  //         );
  //       }
  //       for _ in 0..2 {
  //         opset.next(&mut ()).await;
  //       }
  //     }
  //   });
  // }

  // #[test]
  // fn test_not_ready() {
  //   async_test(async {
  //     let mut opset: OpSet<(), String> = OpSet::default();
  //     assert_eq!(
  //       opset.insert_op(
  //         0,
  //         async {
  //           tokio::task::yield_now().await;
  //           poll_fn(|cx| {
  //             let waker = cx.waker().clone();
  //             std::thread::spawn(|| {
  //               std::thread::sleep(Duration::from_millis(100));
  //               drop(waker);
  //             });
  //             Poll::Ready(())
  //           })
  //           .await;
  //           1_u32
  //         },
  //         |cx, scope, f| {
  //           let v = ready!(f.poll(cx));
  //           Poll::Ready(v.to_string())
  //         }
  //       ),
  //       None
  //     );
  //     opset.next(&mut ()).await;
  //     std::thread::sleep(Duration::from_millis(250));
  //   });
  // }

  // #[test]
  // fn test_clone_waker() {
  //   async_test(async {
  //     let mut opset: OpSet<(), String> = OpSet::default();
  //     assert_eq!(
  //       opset.insert_op(
  //         0,
  //         async {
  //           tokio::task::yield_now().await;
  //           1_u32
  //         },
  //         |cx, scope, f| {
  //           let v = ready!(f.poll(cx));
  //           Poll::Ready(v.to_string())
  //         }
  //       ),
  //       None
  //     );
  //     opset.next(&mut ()).await;
  //   });
  // }

  // #[test]
  // fn test_two_futures() {
  //   async_test(async {
  //     let mut opset: OpSet<(), String> = OpSet::default();
  //     for i in 0..2 {
  //       assert_eq!(
  //         opset.insert_op(
  //           i,
  //           async {
  //             tokio::task::yield_now().await;
  //             1_u32
  //           },
  //           |cx, scope, f| {
  //             let v = ready!(f.poll(cx));
  //             Poll::Ready(v.to_string())
  //           }
  //         ),
  //         None
  //       );
  //     }
  //     for _ in 0..2 {
  //       opset.next(&mut ()).await;
  //     }
  //   });
  // }

  // async fn task() {
  //   tokio::task::yield_now().await;
  //   // _ = File::open("/tmp/maybe-exists-maybe-not").await;
  // }

  // #[test]
  // fn test_many_futures() {
  //   async_test(async {
  //     for i in 0..3 {
  //       eprintln!("{i}");
  //       let mut opset: OpSet<(), ()> = OpSet::default();
  //       for i in 0..10 {
  //         for i in 0..10 {
  //           assert_eq!(
  //             opset.insert_op(
  //               i,
  //               task(),
  //               |cx, scope, f| {
  //                 f.poll(cx)
  //               }
  //             ),
  //             None
  //           );
  //         }
  //         for _ in 0..3 {
  //           opset.next(&mut ()).await;
  //         }
  //       }
  //     }
  //   });
  // }

  // /// Ensure that the tests pass when we use v8 types. These are tested as part of the whole system.
  // #[test]
  // fn test_with_v8() {
  //   let mut opset = OpSet::default();
  //   opset.insert_op(0, async { 1_u32 }, |cx, scope, f| {
  //     let v = ready!(f.poll(cx));
  //     Poll::<v8::Local<v8::Value>>::Ready(
  //       v8::Integer::new_from_unsigned(scope, v).into(),
  //     )
  //   });
  // }
}
