use std::alloc::Layout;
use std::ptr::NonNull;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use parking_lot::lock_api::RawMutex;

use crate::arena::raw_arena::RawArena;

#[cfg(debug_assertions)]
const SIGNATURE: usize = 0x1122334455667788;

pub struct ArenaArc<T> {
  ptr: NonNull<ArenaArcData<T>>,
}

impl<T> ArenaArc<T> {
  const PTR_OFFSET: usize = memoffset::offset_of!(ArenaArc<T>, ptr);

  #[inline(always)]
  unsafe fn data_from_ptr(ptr: *const T) -> NonNull<ArenaArcData<T>> {
    NonNull::new_unchecked((ptr as *const u8).sub(Self::PTR_OFFSET) as _)
  }

  #[inline(always)]
  unsafe fn ptr_from_data(ptr: NonNull<ArenaArcData<T>>) -> *const T {
    (ptr.as_ptr() as *const u8).add(Self::PTR_OFFSET) as _
  }

  #[inline(always)]
  pub fn into_raw(arc: ArenaArc<T>) -> *const T {
    let ptr = arc.ptr;
    std::mem::forget(arc);
    unsafe { Self::ptr_from_data(ptr) }
  }

  #[inline(always)]
  pub fn clone_into_raw(arc: &ArenaArc<T>) -> *const T {
    unsafe {
      arc.ptr.as_ref().ref_count.fetch_add(1, Ordering::Relaxed);
      Self::ptr_from_data(arc.ptr)
    }
  }

  #[inline(always)]
  pub unsafe fn from_raw(ptr: *const T) -> ArenaArc<T> {
    let ptr = Self::data_from_ptr(ptr);

    #[cfg(debug_assertions)]
    debug_assert_eq!(ptr.as_ref().signature, SIGNATURE);
    ArenaArc { ptr }
  }

  #[inline(always)]
  pub unsafe fn clone_from_raw(ptr: *const T) -> ArenaArc<T> {
    let ptr = Self::data_from_ptr(ptr);
    ptr.as_ref().ref_count.fetch_add(1, Ordering::Relaxed);
    ArenaArc { ptr }
  }

  #[inline(always)]
  pub unsafe fn clone_raw_from_raw(ptr: *const T) {
    let ptr = Self::data_from_ptr(ptr);
    ptr.as_ref().ref_count.fetch_add(1, Ordering::Relaxed);
  }

  #[inline(always)]
  pub unsafe fn drop_from_raw(ptr: *const T) {
    let ptr = Self::data_from_ptr(ptr);
    if ptr.as_ref().ref_count.fetch_sub(1, Ordering::Relaxed) == 0 {
      let this = ptr.as_ref();
      (this.deleter)(this.arena_data, ptr.as_ptr() as _);
    }
  }
}

unsafe impl<T: Send + Sync> Send for ArenaArc<T> {}
unsafe impl<T: Send + Sync> Sync for ArenaArc<T> {}

// T is Send + Sync, so ArenaArc is too
static_assertions::assert_impl_all!(ArenaArc<()>: Send, Sync);
// T is !Send & !Sync, so ArenaArc is too
static_assertions::assert_not_impl_any!(ArenaArc<*mut ()>: Send, Sync);

impl<T> ArenaArc<T> {}

impl<T> Drop for ArenaArc<T> {
  fn drop(&mut self) {
    unsafe {
      if self.ptr.as_ref().ref_count.fetch_sub(1, Ordering::Relaxed) == 0 {
        let this = self.ptr.as_ref();
        (this.deleter)(this.arena_data, self.ptr.as_ptr() as _);
      }
    }
  }
}

impl<T> std::ops::Deref for ArenaArc<T> {
  type Target = T;
  fn deref(&self) -> &Self::Target {
    unsafe { &self.ptr.as_ref().data }
  }
}

struct ArenaArcData<T> {
  #[cfg(debug_assertions)]
  signature: usize,
  ref_count: AtomicUsize,
  arena_data: *const (),
  deleter: unsafe fn(*const (), *const ()),
  data: T,
}

pub struct ArenaSharedAtomic<T, const BASE_CAPACITY: usize> {
  ptr: NonNull<ArenaSharedAtomicData<T, BASE_CAPACITY>>,
}

unsafe impl<T, const BASE_CAPACITY: usize> Send
  for ArenaSharedAtomic<T, BASE_CAPACITY>
{
}

// The arena itself may not be shared so that we can guarantee all [`RawArena`]
// access happens on the owning thread.
static_assertions::assert_impl_any!(ArenaSharedAtomic<(), 16>: Send);
static_assertions::assert_not_impl_any!(ArenaSharedAtomic<(), 16>: Sync);

struct ArenaSharedAtomicData<T, const BASE_CAPACITY: usize> {
  mutex: parking_lot::RawMutex,
  raw_arena: RawArena<ArenaArcData<T>, BASE_CAPACITY>,
  ref_count: usize,
}

impl<T, const BASE_CAPACITY: usize> Default
  for ArenaSharedAtomic<T, BASE_CAPACITY>
{
  fn default() -> Self {
    unsafe {
      let ptr = std::alloc::alloc(Layout::new::<
        ArenaSharedAtomicData<T, BASE_CAPACITY>,
      >()) as *mut ArenaSharedAtomicData<T, BASE_CAPACITY>;
      std::ptr::write(
        ptr,
        ArenaSharedAtomicData {
          mutex: parking_lot::RawMutex::INIT,
          raw_arena: Default::default(),
          ref_count: 0,
        },
      );
      Self {
        ptr: NonNull::new_unchecked(ptr),
      }
    }
  }
}

impl<T, const BASE_CAPACITY: usize> ArenaSharedAtomic<T, BASE_CAPACITY> {
  unsafe fn delete(arena: *const (), data: *const ()) {
    let arena = arena as *mut ArenaSharedAtomicData<T, BASE_CAPACITY>;
    let mutex = std::ptr::addr_of!((*arena).mutex);
    while !(*mutex).try_lock() {
      std::thread::yield_now();
    }
    (*arena).raw_arena.recycle(data as _);
    if (*arena).ref_count == 0 {
      std::ptr::drop_in_place(arena);
      std::alloc::dealloc(
        arena as _,
        Layout::new::<ArenaSharedAtomicData<T, BASE_CAPACITY>>(),
      );
    } else {
      (*arena).ref_count -= 1;
      (*mutex).unlock();
    }
  }

  pub fn allocate(&self, data: T) -> ArenaArc<T> {
    let ptr = unsafe {
      let this = self.ptr.as_ptr();
      let mutex = std::ptr::addr_of!((*this).mutex);
      while !(*mutex).try_lock() {
        std::thread::yield_now();
      }
      let ptr = (*this).raw_arena.allocate();
      (*this).ref_count += 1;

      std::ptr::write(
        ptr,
        ArenaArcData {
          #[cfg(debug_assertions)]
          signature: SIGNATURE,
          arena_data: std::mem::transmute(self.ptr),
          ref_count: AtomicUsize::default(),
          deleter: Self::delete,
          data,
        },
      );
      (*mutex).unlock();
      NonNull::new_unchecked(ptr)
    };

    ArenaArc { ptr }
  }
}

impl<T, const BASE_CAPACITY: usize> Drop
  for ArenaSharedAtomic<T, BASE_CAPACITY>
{
  fn drop(&mut self) {
    unsafe {
      let this = self.ptr.as_ptr();
      let mutex = std::ptr::addr_of!((*this).mutex);
      while !(*mutex).try_lock() {
        std::thread::yield_now();
      }
      if (*this).ref_count == 0 {
        std::ptr::drop_in_place(this);
        std::alloc::dealloc(
          this as _,
          Layout::new::<ArenaSharedAtomicData<T, BASE_CAPACITY>>(),
        );
      } else {
        (*this).ref_count -= 1;
        (*mutex).unlock();
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::cell::RefCell;
  use std::sync::Arc;
  use std::sync::Mutex;

  #[test]
  fn test_raw() {
    let arena: ArenaSharedAtomic<RefCell<usize>, 16> = Default::default();
    let arc = arena.allocate(Default::default());
    let raw = ArenaArc::into_raw(arc);
    _ = unsafe { ArenaArc::from_raw(raw) };
  }

  #[test]
  fn test_clone_into_raw() {
    let arena: ArenaSharedAtomic<RefCell<usize>, 16> = Default::default();
    let arc = arena.allocate(Default::default());
    let raw = ArenaArc::clone_into_raw(&arc);
    _ = unsafe { ArenaArc::from_raw(raw) };
  }

  #[test]
  fn test_allocate_drop_arc_first() {
    let arena: ArenaSharedAtomic<RefCell<usize>, 16> = Default::default();
    let arc = arena.allocate(Default::default());
    *arc.borrow_mut() += 1;
    drop(arc);
    drop(arena);
  }

  #[test]
  fn test_allocate_drop_arena_first() {
    let arena: ArenaSharedAtomic<RefCell<usize>, 16> = Default::default();
    let arc = arena.allocate(Default::default());
    *arc.borrow_mut() += 1;
    drop(arena);
    drop(arc);
  }

  #[test]
  fn test_threaded() {
    let arena: Arc<Mutex<ArenaSharedAtomic<RefCell<usize>, 16>>> =
      Default::default();
    const THREADS: usize = 20;
    const ITERATIONS: usize = 100;

    let mut handles = Vec::new();
    let barrier = Arc::new(std::sync::Barrier::new(THREADS));

    for _ in 0..THREADS {
      let arena = Arc::clone(&arena);
      let barrier = Arc::clone(&barrier);

      let handle = std::thread::spawn(move || {
        barrier.wait();

        for _ in 0..ITERATIONS {
          let arc = arena.lock().unwrap().allocate(RefCell::new(0));
          *arc.borrow_mut() += 1;
          drop(arc); // Ensure the Arc is dropped at the end of the iteration
        }
      });

      handles.push(handle);
    }

    for handle in handles {
      handle.join().unwrap();
    }
  }
}
