use std::alloc::Layout;
use std::pin::Pin;
use std::ptr::NonNull;
use std::future::Future;

use crate::arena::raw_arena::RawArena;

#[cfg(debug_assertions)]
const SIGNATURE: usize = 0x8877665544332211;

pub struct ArenaBox<T: 'static> {
  ptr: NonNull<ArenaBoxData<T>>,
}

impl<T> Unpin for ArenaBox<T> {}

struct ArenaBoxData<T> {
  #[cfg(debug_assertions)]
  signature: usize,
  arena_data: *const (),
  deleter: unsafe fn(&mut Self),
  data: T,
}

impl<T: 'static> ArenaBox<T> {
  /// Offset of the `ptr` field within the `ArenaBox` struct.
  const PTR_OFFSET: usize = memoffset::offset_of!(ArenaBox<T>, ptr);

  /// Constructs a `NonNull` reference to `ArenaBoxData` from a raw pointer to `T`.
  #[inline(always)]
  unsafe fn data_from_ptr(ptr: *mut T) -> NonNull<ArenaBoxData<T>> {
    NonNull::new_unchecked((ptr as *mut u8).sub(Self::PTR_OFFSET) as _)
  }

  /// Obtains a raw pointer to `T` from a `NonNull` reference to `ArenaBoxData`.  
  #[inline(always)]
  unsafe fn ptr_from_data(ptr: NonNull<ArenaBoxData<T>>) -> *mut T {
    (ptr.as_ptr() as *mut u8).add(Self::PTR_OFFSET) as _
  }

  /// Transforms an `ArenaBox` into a raw pointer to `T` and forgets it.
  #[inline(always)]
  pub fn into_raw(mut alloc: ArenaBox<T>) -> *mut T {
    let ptr = NonNull::from(alloc.data_mut());
    std::mem::forget(alloc);
    unsafe { Self::ptr_from_data(ptr) }
  }

  /// Constructs an `ArenaBox` instance from a raw pointer to `T`.
  /// 
  /// # Safety
  ///
  /// This function constructs an `ArenaBox` from a raw pointer, assuming the pointer is valid and properly aligned.
  #[inline(always)]
  pub unsafe fn from_raw(ptr: *mut T) -> ArenaBox<T> {
    let mut ptr = Self::data_from_ptr(ptr);

    #[cfg(debug_assertions)]
    debug_assert_eq!(ptr.as_mut().signature, SIGNATURE);
    ArenaBox { ptr }
  }
}

// This Box cannot be sent between threads
static_assertions::assert_not_impl_any!(ArenaBox<()>: Send, Sync);

impl<T> ArenaBox<T> {
  fn data(&self) -> &ArenaBoxData<T> {
    unsafe { self.ptr.as_ref() }
  }

  fn data_mut(&mut self) -> &mut ArenaBoxData<T> {
    unsafe { self.ptr.as_mut() }
  }
}

impl<T> Drop for ArenaBox<T> {
  fn drop(&mut self) {
    unsafe {
      (self.data().deleter)(self.data_mut());
    }
  }
}

impl<T> std::ops::Deref for ArenaBox<T> {
  type Target = T;
  fn deref(&self) -> &Self::Target {
    &self.data().data
  }
}

impl<T> std::ops::DerefMut for ArenaBox<T> {
  fn deref_mut(&mut self) -> &mut Self::Target {
    &mut self.data_mut().data
  }
}

impl<F, R> std::future::Future for ArenaBox<F>
where
  F: Future<Output = R>,
{
  type Output = R;

  #[inline(always)]
  fn poll(
    mut self: Pin<&mut Self>,
    cx: &mut std::task::Context<'_>,
  ) -> std::task::Poll<Self::Output> {
    unsafe { F::poll(Pin::new_unchecked(&mut self.data_mut().data), cx) }
  }
}

/// An arena-based unique ownership container allowing allocation
/// and deallocation of objects with exclusive ownership semantics.
///
/// `ArenaUnique` provides exclusive ownership semantics similar to
/// a `Box`. It utilizes a `RawArena` for allocation and
/// deallocation of objects, maintaining the sole ownership of the
/// allocated data and enabling safe cleanup when the `ArenaUnique`
/// instance is dropped.
///
/// This container guarantees exclusive access to the allocated data
/// within the arena, allowing single-threaded operations while
/// efficiently managing memory and ensuring cleanup on drop.
pub struct ArenaUnique<T, const BASE_CAPACITY: usize> {
  ptr: NonNull<ArenaUniqueData<T, BASE_CAPACITY>>,
}

// The arena itself may not be shared so that we can guarantee all [`RawArena`]
// access happens on the owning thread.
static_assertions::assert_not_impl_any!(ArenaUnique<(), 16>: Send, Sync);

struct ArenaUniqueData<T, const BASE_CAPACITY: usize> {
  raw_arena: RawArena<ArenaBoxData<T>, BASE_CAPACITY>,
  ref_count: usize,
}

impl<T, const BASE_CAPACITY: usize> Default for ArenaUnique<T, BASE_CAPACITY> {
  fn default() -> Self {
    unsafe {
      let ptr = std::alloc::alloc(Layout::new::<
        ArenaUniqueData<T, BASE_CAPACITY>,
      >()) as *mut ArenaUniqueData<T, BASE_CAPACITY>;
      std::ptr::write(
        ptr,
        ArenaUniqueData {
          raw_arena: Default::default(),
          ref_count: 1,
        },
      );
      Self {
        ptr: NonNull::new_unchecked(ptr),
      }
    }
  }
}

impl<T, const BASE_CAPACITY: usize> ArenaUnique<T, BASE_CAPACITY> {
  /// Deletes the data associated with an `ArenaBox` from the arena.
  unsafe fn delete(data: &mut ArenaBoxData<T>) {
    let arena = data.arena_data as *mut ArenaUniqueData<T, BASE_CAPACITY>;
    (*arena).raw_arena.recycle(data as _);
    if (*arena).ref_count == 0 {
      std::ptr::drop_in_place(arena);
      std::alloc::dealloc(
        arena as _,
        Layout::new::<ArenaUniqueData<T, BASE_CAPACITY>>(),
      );
    } else {
      (*arena).ref_count -= 1;
    }
  }

  /// Allocates a new data instance of type `T` within the arena, encapsulating it within an `ArenaBox`.
  ///
  /// This method creates a new instance of type `T` within the `RawArena`, which is the underlying memory
  /// allocation mechanism used by the `ArenaUnique`. The provided `data` is initialized within the arena,
  /// and an `ArenaBox` is returned to manage this allocated data. The `ArenaBox` serves as a reference to
  /// the allocated data within the arena, providing safe access and management of the stored value.
  ///
  /// # Example
  ///
  /// ```rust
  /// # use deno_core::arena::ArenaUnique;
  ///
  /// // Define a struct that will be allocated within the arena
  /// struct MyStruct {
  ///   data: usize,
  /// }
  ///
  /// // Create a new instance of ArenaUnique with a specified base capacity
  /// let arena: ArenaUnique<MyStruct, 16> = ArenaUnique::default();
  ///
  /// // Allocate a new MyStruct instance within the arena
  /// let data_instance = MyStruct { data: 42 };
  /// let allocated_box = arena.allocate(data_instance);
  ///
  /// // Now, allocated_box can be used as a managed reference to the allocated data
  /// assert_eq!(allocated_box.data, 42); // Validate the data stored in the allocated box
  /// ```
  pub fn allocate(&self, data: T) -> ArenaBox<T> {
    let ptr = unsafe {
      let this = self.ptr.as_ptr();
      let ptr = (*this).raw_arena.allocate();
      (*this).ref_count += 1;

      std::ptr::write(
        ptr,
        ArenaBoxData {
          #[cfg(debug_assertions)]
          signature: SIGNATURE,
          arena_data: std::mem::transmute(self.ptr),
          deleter: Self::delete,
          data,
        },
      );
      std::mem::transmute(&mut *ptr)
    };

    ArenaBox { ptr }
  }
}

impl<T, const BASE_CAPACITY: usize> Drop for ArenaUnique<T, BASE_CAPACITY> {
  fn drop(&mut self) {
    unsafe {
      let this = self.ptr.as_ptr();
      if (*this).ref_count == 0 {
        std::ptr::drop_in_place(this);
        std::alloc::dealloc(
          this as _,
          Layout::new::<ArenaUniqueData<T, BASE_CAPACITY>>(),
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
    let arena: ArenaUnique<RefCell<usize>, 16> = Default::default();
    let arc = arena.allocate(Default::default());
    let raw = ArenaBox::into_raw(arc);
    _ = unsafe { ArenaBox::from_raw(raw) };
  }

  #[test]
  fn test_allocate_drop_box_first() {
    let arena: ArenaUnique<RefCell<usize>, 16> = Default::default();
    let alloc = arena.allocate(Default::default());
    *alloc.borrow_mut() += 1;
    drop(alloc);
    drop(arena);
  }

  #[test]
  fn test_allocate_drop_arena_first() {
    let arena: ArenaUnique<RefCell<usize>, 16> = Default::default();
    let alloc = arena.allocate(Default::default());
    *alloc.borrow_mut() += 1;
    drop(arena);
    drop(alloc);
  }
}
