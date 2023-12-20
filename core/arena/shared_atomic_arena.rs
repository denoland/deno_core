// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use std::alloc::Layout;
use std::ptr::NonNull;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use parking_lot::lock_api::RawMutex;

use crate::arena::raw_arena::RawArena;

#[cfg(debug_assertions)]
const SIGNATURE: usize = 0x1122334455667788;

/// Represents an atomic reference-counted pointer into an arena-allocated object.
pub struct ArenaArc<T> {
  ptr: NonNull<ArenaArcData<T>>,
}

impl<T> ArenaArc<T> {
  /// Offset of the `ptr` field within the `ArenaArc` struct.
  const PTR_OFFSET: usize = memoffset::offset_of!(ArenaArc<T>, ptr);

  /// Converts a raw pointer to the data into a `NonNull` pointer to `ArenaArcData`.
  ///
  /// # Safety
  ///
  /// This function assumes that the input `ptr` points to the data within an `ArenaArc` object.
  /// Improper usage may result in undefined behavior.
  #[inline(always)]
  unsafe fn data_from_ptr(ptr: *const T) -> NonNull<ArenaArcData<T>> {
    NonNull::new_unchecked((ptr as *const u8).sub(Self::PTR_OFFSET) as _)
  }

  /// Converts a `NonNull` pointer to `ArenaArcData` into a raw pointer to the data.
  ///
  /// # Safety
  ///
  /// This function assumes that the input `ptr` is a valid `NonNull` pointer to `ArenaArcData`.
  /// Improper usage may result in undefined behavior.
  #[inline(always)]
  unsafe fn ptr_from_data(ptr: NonNull<ArenaArcData<T>>) -> *const T {
    (ptr.as_ptr() as *const u8).add(Self::PTR_OFFSET) as _
  }

  /// Consumes the `ArenaArc`, forgetting it, and returns a raw pointer to the contained data.
  ///
  /// # Safety
  ///
  /// This function returns a raw pointer without managing the memory, potentially leading to
  /// memory leaks if the pointer is not properly handled or deallocated.
  #[inline(always)]
  pub fn into_raw(arc: ArenaArc<T>) -> *const T {
    let ptr = arc.ptr;
    std::mem::forget(arc);
    unsafe { Self::ptr_from_data(ptr) }
  }

  /// Clones the `ArenaArc` reference, increments its reference count, and returns a raw pointer to the contained data.
  ///
  /// This function increments the reference count of the `ArenaArc`.
  #[inline(always)]
  pub fn clone_into_raw(arc: &ArenaArc<T>) -> *const T {
    unsafe {
      arc.ptr.as_ref().ref_count.fetch_add(1, Ordering::Relaxed);
      Self::ptr_from_data(arc.ptr)
    }
  }

  /// Constructs an `ArenaArc` from a raw pointer to the contained data.
  ///
  /// This function safely constructs an `ArenaArc` from a raw pointer, assuming the pointer is
  /// valid, properly aligned, and was originally created by `into_raw` or `clone_into_raw`.
  ///
  /// # Safety
  ///
  /// This function assumes the provided `ptr` is a valid raw pointer to the data within an `ArenaArc`
  /// object. Misuse may lead to undefined behavior, memory unsafety, or data corruption.
  #[inline(always)]
  pub unsafe fn from_raw(ptr: *const T) -> ArenaArc<T> {
    let ptr = Self::data_from_ptr(ptr);

    #[cfg(debug_assertions)]
    debug_assert_eq!(ptr.as_ref().signature, SIGNATURE);
    ArenaArc { ptr }
  }

  /// Clones an `ArenaArc` reference from a raw pointer and increments its reference count.
  ///
  /// This method increments the reference count of the `ArenaArc` instance
  /// associated with the provided raw pointer, allowing multiple references
  /// to the same allocated data.
  ///
  /// # Safety
  ///
  /// This function assumes that the provided `ptr` is a valid raw pointer
  /// to the data within an `ArenaArc` object. Improper usage may lead
  /// to memory unsafety or data corruption.
  #[inline(always)]
  pub unsafe fn clone_from_raw(ptr: *const T) -> ArenaArc<T> {
    let ptr = Self::data_from_ptr(ptr);
    ptr.as_ref().ref_count.fetch_add(1, Ordering::Relaxed);
    ArenaArc { ptr }
  }

  /// Increments the reference count associated with the raw pointer to an `ArenaArc`-managed data.
  ///
  /// This method manually increases the reference count of the `ArenaArc` instance
  /// associated with the provided raw pointer. It allows incrementing the reference count
  /// without constructing a full `ArenaArc` instance, ideal for scenarios where direct
  /// manipulation of raw pointers is required.
  ///
  /// # Safety
  ///
  /// This method bypasses some safety checks enforced by the `ArenaArc` type. Incorrect usage
  /// or mishandling of raw pointers might lead to memory unsafety or data corruption.
  /// Use with caution and ensure proper handling of associated data.
  #[inline(always)]
  pub unsafe fn clone_raw_from_raw(ptr: *const T) {
    let ptr = Self::data_from_ptr(ptr);
    ptr.as_ref().ref_count.fetch_add(1, Ordering::Relaxed);
  }

  /// Drops the `ArenaArc` reference pointed to by the raw pointer.
  ///
  /// If the reference count drops to zero, the associated data is returned to the arena.
  ///
  /// # Safety
  ///
  /// This function assumes that the provided `ptr` is a valid raw pointer
  /// to the data within an `ArenaArc` object. Improper usage may lead
  /// to memory unsafety or data corruption.
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

/// Data structure containing metadata and the actual data within the `ArenaArc`.
struct ArenaArcData<T> {
  #[cfg(debug_assertions)]
  signature: usize,
  ref_count: AtomicUsize,
  arena_data: *const (),
  deleter: unsafe fn(*const (), *const ()),
  data: T,
}

/// An atomic reference-counted pointer into an arena-allocated object
/// with thread-safe allocation and deallocation capabilities.
///
/// This structure ensures atomic access and safe sharing of allocated
/// data across multiple threads while maintaining reference counting
/// to manage the memory deallocation when no longer needed.
///
/// It combines a thread-safe `RawArena` for allocation and deallocation
/// and provides a mutex to guarantee exclusive access to the internal
/// data for safe multi-threaded operation.
///
/// The `ArenaSharedAtomic` allows multiple threads to allocate, share,
/// and deallocate objects within the arena, ensuring safety and atomicity
/// during these operations.
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

/// Data structure containing a mutex and the `RawArena` for atomic access in the `ArenaSharedAtomic`.
struct ArenaSharedAtomicData<T, const BASE_CAPACITY: usize> {
  /// A mutex ensuring thread-safe access to the internal raw arena and refcount.
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

  /// Allocates a new object in the arena and returns an `ArenaArc` pointing to it.
  ///
  /// This method creates a new instance of type `T` within the `RawArena`. The provided `data`
  /// is initialized within the arena, and an `ArenaArc` is returned to manage this allocated data.
  /// The `ArenaArc` serves as an atomic, reference-counted pointer to the allocated data within
  /// the arena, ensuring safe concurrent access across multiple threads while maintaining the
  /// reference count for memory management.
  ///
  /// The allocation process employs a mutex to ensure thread-safe access to the arena, allowing
  /// only one thread at a time to modify the internal state, including allocating and deallocating memory.
  ///
  /// # Safety
  ///
  /// The provided `data` is allocated within the arena and managed by the `ArenaArc`. Improper handling
  /// or misuse of the returned `ArenaArc` pointer may lead to memory leaks or memory unsafety.
  ///
  /// # Example
  ///
  /// ```rust
  /// # use deno_core::arena::ArenaSharedAtomic;
  ///
  /// // Define a struct that will be allocated within the arena
  /// struct MyStruct {
  ///     data: usize,
  /// }
  ///
  /// // Create a new instance of ArenaSharedAtomic with a specified base capacity
  /// let arena: ArenaSharedAtomic<MyStruct, 16> = ArenaSharedAtomic::default();
  ///
  /// // Allocate a new MyStruct instance within the arena
  /// let data_instance = MyStruct { data: 42 };
  /// let allocated_arc = arena.allocate(data_instance);
  ///
  /// // Now, allocated_arc can be used as a managed reference to the allocated data
  /// assert_eq!(allocated_arc.data, 42); // Validate the data stored in the allocated arc
  /// ```
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
