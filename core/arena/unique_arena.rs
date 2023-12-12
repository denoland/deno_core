use std::alloc::Layout;
use std::pin::Pin;
use std::ptr::NonNull;

use futures::Future;

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
  const PTR_OFFSET: usize = memoffset::offset_of!(ArenaBox<T>, ptr);

  #[inline(always)]
  unsafe fn data_from_ptr(ptr: *mut T) -> NonNull<ArenaBoxData<T>> {
    NonNull::new_unchecked((ptr as *mut u8).sub(Self::PTR_OFFSET) as _)
  }

  #[inline(always)]
  unsafe fn ptr_from_data(ptr: NonNull<ArenaBoxData<T>>) -> *mut T {
    (ptr.as_ptr() as *mut u8).add(Self::PTR_OFFSET) as _
  }

  #[inline(always)]
  pub fn into_raw(mut alloc: ArenaBox<T>) -> *mut T {
    let ptr = NonNull::from(alloc.data_mut());
    std::mem::forget(alloc);
    unsafe { Self::ptr_from_data(ptr) }
  }

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
          ref_count: 0,
        },
      );
      Self {
        ptr: NonNull::new_unchecked(ptr),
      }
    }
  }
}

impl<T, const BASE_CAPACITY: usize> ArenaUnique<T, BASE_CAPACITY> {
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
