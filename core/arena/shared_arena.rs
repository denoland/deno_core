// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use std::alloc::Layout;
use std::cell::Cell;
use std::ptr::NonNull;

use crate::arena::raw_arena::RawArena;

#[cfg(debug_assertions)]
const SIGNATURE: usize = 0x1122334455667788;

pub struct ArenaSharedReservation<T, const BASE_CAPACITY: usize>(
  *mut ArenaRcData<T>,
);

impl<T, const BASE_CAPACITY: usize> Drop
  for ArenaSharedReservation<T, BASE_CAPACITY>
{
  fn drop(&mut self) {
    panic!("A reservation must be completed or forgotten")
  }
}

/// Represents an atomic reference-counted pointer into an arena-allocated object.
pub struct ArenaRc<T> {
  ptr: NonNull<ArenaRcData<T>>,
}

static_assertions::assert_not_impl_any!(ArenaRc<()>: Send, Sync);

impl<T> ArenaRc<T> {
  /// Offset of the `ptr` field within the `ArenaRc` struct.
  const PTR_OFFSET: usize = memoffset::offset_of!(ArenaRc<T>, ptr);

  /// Converts a raw pointer to the data into a `NonNull` pointer to `ArenaRcData`.
  ///
  /// # Safety
  ///
  /// This function assumes that the input `ptr` points to the data within an `ArenaRc` object.
  /// Improper usage may result in undefined behavior.
  #[inline(always)]
  unsafe fn data_from_ptr(ptr: *const T) -> NonNull<ArenaRcData<T>> {
    NonNull::new_unchecked((ptr as *const u8).sub(Self::PTR_OFFSET) as _)
  }

  /// Converts a `NonNull` pointer to `ArenaRcData` into a raw pointer to the data.
  ///
  /// # Safety
  ///
  /// This function assumes that the input `ptr` is a valid `NonNull` pointer to `ArenaRcData`.
  /// Improper usage may result in undefined behavior.
  #[inline(always)]
  unsafe fn ptr_from_data(ptr: NonNull<ArenaRcData<T>>) -> *const T {
    (ptr.as_ptr() as *const u8).add(Self::PTR_OFFSET) as _
  }

  /// Consumes the `ArenaRc`, forgetting it, and returns a raw pointer to the contained data.
  ///
  /// # Safety
  ///
  /// This function returns a raw pointer without managing the memory, potentially leading to
  /// memory leaks if the pointer is not properly handled or deallocated.
  #[inline(always)]
  pub fn into_raw(arc: ArenaRc<T>) -> *const T {
    let ptr = arc.ptr;
    std::mem::forget(arc);
    unsafe { Self::ptr_from_data(ptr) }
  }

  /// Clones the `ArenaRc` reference, increments its reference count, and returns a raw pointer to the contained data.
  ///
  /// This function increments the reference count of the `ArenaRc`.
  #[inline(always)]
  pub fn clone_into_raw(arc: &ArenaRc<T>) -> *const T {
    unsafe {
      let ptr = arc.ptr;
      ptr.as_ref().ref_count.set(ptr.as_ref().ref_count.get() + 1);
      Self::ptr_from_data(arc.ptr)
    }
  }

  /// Constructs an `ArenaRc` from a raw pointer to the contained data.
  ///
  /// This function safely constructs an `ArenaRc` from a raw pointer, assuming the pointer is
  /// valid, properly aligned, and was originally created by `into_raw` or `clone_into_raw`.
  ///
  /// # Safety
  ///
  /// This function assumes the provided `ptr` is a valid raw pointer to the data within an `ArenaRc`
  /// object. Misuse may lead to undefined behavior, memory unsafety, or data corruption.
  #[inline(always)]
  pub unsafe fn from_raw(ptr: *const T) -> ArenaRc<T> {
    let ptr = Self::data_from_ptr(ptr);

    #[cfg(debug_assertions)]
    debug_assert_eq!(ptr.as_ref().signature, SIGNATURE);
    ArenaRc { ptr }
  }

  /// Clones an `ArenaRc` reference from a raw pointer and increments its reference count.
  ///
  /// This method increments the reference count of the `ArenaRc` instance
  /// associated with the provided raw pointer, allowing multiple references
  /// to the same allocated data.
  ///
  /// # Safety
  ///
  /// This function assumes that the provided `ptr` is a valid raw pointer
  /// to the data within an `ArenaRc` object. Improper usage may lead
  /// to memory unsafety or data corruption.
  #[inline(always)]
  pub unsafe fn clone_from_raw(ptr: *const T) -> ArenaRc<T> {
    let ptr = Self::data_from_ptr(ptr);
    ptr.as_ref().ref_count.set(ptr.as_ref().ref_count.get() + 1);
    ArenaRc { ptr }
  }

  /// Increments the reference count associated with the raw pointer to an `ArenaRc`-managed data.
  ///
  /// This method manually increases the reference count of the `ArenaRc` instance
  /// associated with the provided raw pointer. It allows incrementing the reference count
  /// without constructing a full `ArenaRc` instance, ideal for scenarios where direct
  /// manipulation of raw pointers is required.
  ///
  /// # Safety
  ///
  /// This method bypasses some safety checks enforced by the `ArenaRc` type. Incorrect usage
  /// or mishandling of raw pointers might lead to memory unsafety or data corruption.
  /// Use with caution and ensure proper handling of associated data.
  #[inline(always)]
  pub unsafe fn clone_raw_from_raw(ptr: *const T) {
    let ptr = Self::data_from_ptr(ptr);
    ptr.as_ref().ref_count.set(ptr.as_ref().ref_count.get() + 1);
  }

  /// Drops the `ArenaRc` reference pointed to by the raw pointer.
  ///
  /// If the reference count drops to zero, the associated data is returned to the arena.
  ///
  /// # Safety
  ///
  /// This function assumes that the provided `ptr` is a valid raw pointer
  /// to the data within an `ArenaRc` object. Improper usage may lead
  /// to memory unsafety or data corruption.
  #[inline(always)]
  pub unsafe fn drop_from_raw(ptr: *const T) {
    let ptr = Self::data_from_ptr(ptr);
    let ref_count = ptr.as_ref().ref_count.get();
    if ref_count == 0 {
      let this = ptr.as_ref();
      (this.deleter)(this.arena_data, ptr.as_ptr() as _);
    } else {
      ptr.as_ref().ref_count.set(ref_count - 1);
    }
  }
}

impl<T> ArenaRc<T> {}

impl<T> Drop for ArenaRc<T> {
  fn drop(&mut self) {
    unsafe {
      let ref_count = self.ptr.as_ref().ref_count.get();
      if ref_count == 0 {
        let this = self.ptr.as_ref();
        (this.deleter)(this.arena_data, self.ptr.as_ptr() as _);
      } else {
        self.ptr.as_ref().ref_count.set(ref_count - 1);
      }
    }
  }
}

impl<T> std::ops::Deref for ArenaRc<T> {
  type Target = T;
  fn deref(&self) -> &Self::Target {
    unsafe { &self.ptr.as_ref().data }
  }
}

/// Data structure containing metadata and the actual data within the `ArenaRc`.
struct ArenaRcData<T> {
  #[cfg(debug_assertions)]
  signature: usize,
  ref_count: Cell<usize>,
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
/// The `ArenaShared` allows multiple threads to allocate, share,
/// and deallocate objects within the arena, ensuring safety and atomicity
/// during these operations.
pub struct ArenaShared<T, const BASE_CAPACITY: usize> {
  ptr: NonNull<ArenaSharedData<T, BASE_CAPACITY>>,
}

static_assertions::assert_not_impl_any!(ArenaShared<(), 16>: Send, Sync);

/// Data structure containing a mutex and the `RawArena` for atomic access in the `ArenaShared`.
struct ArenaSharedData<T, const BASE_CAPACITY: usize> {
  raw_arena: RawArena<ArenaRcData<T>, BASE_CAPACITY>,
  ref_count: usize,
}

impl<T, const BASE_CAPACITY: usize> Default for ArenaShared<T, BASE_CAPACITY> {
  fn default() -> Self {
    unsafe {
      let ptr = std::alloc::alloc(Layout::new::<
        ArenaSharedData<T, BASE_CAPACITY>,
      >()) as *mut ArenaSharedData<T, BASE_CAPACITY>;
      std::ptr::write(
        ptr,
        ArenaSharedData {
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

impl<T, const BASE_CAPACITY: usize> ArenaShared<T, BASE_CAPACITY> {
  unsafe fn delete(arena: *const (), data: *const ()) {
    let arena = arena as *mut ArenaSharedData<T, BASE_CAPACITY>;
    (*arena).raw_arena.recycle(data as _);
    if (*arena).ref_count == 0 {
      std::ptr::drop_in_place(arena);
      std::alloc::dealloc(
        arena as _,
        Layout::new::<ArenaSharedData<T, BASE_CAPACITY>>(),
      );
    } else {
      (*arena).ref_count -= 1;
    }
  }

  /// Allocates a new object in the arena and returns an `ArenaRc` pointing to it.
  ///
  /// This method creates a new instance of type `T` within the `RawArena`. The provided `data`
  /// is initialized within the arena, and an `ArenaRc` is returned to manage this allocated data.
  /// The `ArenaRc` serves as an atomic, reference-counted pointer to the allocated data within
  /// the arena, ensuring safe concurrent access across multiple threads while maintaining the
  /// reference count for memory management.
  ///
  /// The allocation process employs a mutex to ensure thread-safe access to the arena, allowing
  /// only one thread at a time to modify the internal state, including allocating and deallocating memory.
  ///
  /// # Safety
  ///
  /// The provided `data` is allocated within the arena and managed by the `ArenaRc`. Improper handling
  /// or misuse of the returned `ArenaRc` pointer may lead to memory leaks or memory unsafety.
  ///
  /// # Example
  ///
  /// ```rust
  /// # use deno_core::arena::ArenaShared;
  ///
  /// // Define a struct that will be allocated within the arena
  /// struct MyStruct {
  ///     data: usize,
  /// }
  ///
  /// // Create a new instance of ArenaShared with a specified base capacity
  /// let arena: ArenaShared<MyStruct, 16> = ArenaShared::default();
  ///
  /// // Allocate a new MyStruct instance within the arena
  /// let data_instance = MyStruct { data: 42 };
  /// let allocated_arc = arena.allocate(data_instance);
  ///
  /// // Now, allocated_arc can be used as a managed reference to the allocated data
  /// assert_eq!(allocated_arc.data, 42); // Validate the data stored in the allocated arc
  /// ```
  pub fn allocate(&self, data: T) -> ArenaRc<T> {
    let ptr = unsafe {
      let this = self.ptr.as_ptr();
      let ptr = (*this).raw_arena.allocate();
      (*this).ref_count += 1;

      std::ptr::write(
        ptr,
        ArenaRcData {
          #[cfg(debug_assertions)]
          signature: SIGNATURE,
          arena_data: std::mem::transmute(self.ptr),
          ref_count: Default::default(),
          deleter: Self::delete,
          data,
        },
      );
      NonNull::new_unchecked(ptr)
    };

    ArenaRc { ptr }
  }

  /// Allocates a new object in the arena and returns an `ArenaRc` pointing to it. If no space
  /// is available, returns the original object.
  ///
  /// This method creates a new instance of type `T` within the `RawArena`. The provided `data`
  /// is initialized within the arena, and an `ArenaRc` is returned to manage this allocated data.
  /// The `ArenaRc` serves as an atomic, reference-counted pointer to the allocated data within
  /// the arena, ensuring safe concurrent access across multiple threads while maintaining the
  /// reference count for memory management.
  ///
  /// The allocation process employs a mutex to ensure thread-safe access to the arena, allowing
  /// only one thread at a time to modify the internal state, including allocating and deallocating memory.
  pub fn allocate_if_space(&self, data: T) -> Result<ArenaRc<T>, T> {
    let ptr = unsafe {
      let this = self.ptr.as_ptr();
      let ptr = (*this).raw_arena.allocate_if_space();
      if ptr.is_null() {
        return Err(data);
      }
      (*this).ref_count += 1;

      std::ptr::write(
        ptr,
        ArenaRcData {
          #[cfg(debug_assertions)]
          signature: SIGNATURE,
          arena_data: std::mem::transmute(self.ptr),
          ref_count: Default::default(),
          deleter: Self::delete,
          data,
        },
      );
      NonNull::new_unchecked(ptr)
    };

    Ok(ArenaRc { ptr })
  }

  pub unsafe fn reserve_space(
    &self,
  ) -> Option<ArenaSharedReservation<T, BASE_CAPACITY>> {
    let this = self.ptr.as_ptr();
    let ptr = (*this).raw_arena.allocate_if_space();
    if ptr.is_null() {
      return None;
    }
    (*this).ref_count += 1;
    Some(ArenaSharedReservation(ptr))
  }

  pub unsafe fn forget_reservation(
    &self,
    reservation: ArenaSharedReservation<T, BASE_CAPACITY>,
  ) {
    let ptr = reservation.0;
    std::mem::forget(reservation);
    let this = self.ptr.as_ptr();
    (*this).ref_count -= 1;
    (*this).raw_arena.recycle_without_drop(ptr);
  }

  pub unsafe fn complete_reservation(
    &self,
    reservation: ArenaSharedReservation<T, BASE_CAPACITY>,
    data: T,
  ) -> ArenaRc<T> {
    let ptr = reservation.0;
    std::mem::forget(reservation);
    let ptr = {
      std::ptr::write(
        ptr,
        ArenaRcData {
          #[cfg(debug_assertions)]
          signature: SIGNATURE,
          arena_data: std::mem::transmute(self.ptr),
          ref_count: Default::default(),
          deleter: Self::delete,
          data,
        },
      );
      NonNull::new_unchecked(ptr)
    };

    ArenaRc { ptr }
  }
}

impl<T, const BASE_CAPACITY: usize> Drop for ArenaShared<T, BASE_CAPACITY> {
  fn drop(&mut self) {
    unsafe {
      let this = self.ptr.as_ptr();
      if (*this).ref_count == 0 {
        std::ptr::drop_in_place(this);
        std::alloc::dealloc(
          this as _,
          Layout::new::<ArenaSharedData<T, BASE_CAPACITY>>(),
        );
      } else {
        (*this).ref_count -= 1;
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::cell::RefCell;

  #[test]
  fn test_raw() {
    let arena: ArenaShared<RefCell<usize>, 16> = Default::default();
    let arc = arena.allocate(Default::default());
    let raw = ArenaRc::into_raw(arc);
    _ = unsafe { ArenaRc::from_raw(raw) };
  }

  #[test]
  fn test_clone_into_raw() {
    let arena: ArenaShared<RefCell<usize>, 16> = Default::default();
    let arc = arena.allocate(Default::default());
    let raw = ArenaRc::clone_into_raw(&arc);
    _ = unsafe { ArenaRc::from_raw(raw) };
  }

  #[test]
  fn test_allocate_drop_arc_first() {
    let arena: ArenaShared<RefCell<usize>, 16> = Default::default();
    let arc = arena.allocate(Default::default());
    *arc.borrow_mut() += 1;
    drop(arc);
    drop(arena);
  }

  #[test]
  fn test_allocate_drop_arena_first() {
    let arena: ArenaShared<RefCell<usize>, 16> = Default::default();
    let arc = arena.allocate(Default::default());
    *arc.borrow_mut() += 1;
    drop(arena);
    drop(arc);
  }
}
