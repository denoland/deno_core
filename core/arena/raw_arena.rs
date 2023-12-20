// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use std::alloc::handle_alloc_error;
use std::alloc::Layout;
use std::cell::Cell;
use std::mem::ManuallyDrop;

/// A very-`unsafe`, arena for raw pointers that falls back to raw allocation when full. This
/// should be used with great care, and ideally you should only be using the higher-level arenas
/// built on top of this.
pub struct RawArena<T, const BASE_CAPACITY: usize> {
  alloc: *mut RawArenaEntry<T>,
  past_alloc_end: *mut RawArenaEntry<T>,
  max: Cell<*mut RawArenaEntry<T>>,
  next: Cell<*mut RawArenaEntry<T>>,
  allocated: Cell<usize>,
}

/// The [`RawArena`] is [`Send`], but not [`Sync`].
unsafe impl<T, const BASE_CAPACITY: usize> Send for RawArena<T, BASE_CAPACITY> {}

static_assertions::assert_impl_one!(RawArena<(), 16>: Send);
static_assertions::assert_not_impl_any!(RawArena<(), 16>: Sync);

union RawArenaEntry<T> {
  /// If this is a vacant entry, points to the next entry.
  next: *mut RawArenaEntry<T>,
  /// If this is a valid entry, contains the raw data.
  value: ManuallyDrop<T>,
}

impl<T, const BASE_CAPACITY: usize> RawArena<T, BASE_CAPACITY> {
  // TODO(mmastrac): const when https://github.com/rust-lang/rust/issues/67521 is fixed
  fn layout() -> Layout {
    match Layout::array::<RawArenaEntry<T>>(BASE_CAPACITY) {
      Ok(l) => l,
      _ => panic!("Zero-sized objects are not supported"),
    }
  }

  /// Helper method to transmute internal pointers.
  ///
  /// # Safety
  ///
  /// For internal use.
  #[inline(always)]
  unsafe fn entry_to_data(entry: *mut RawArenaEntry<T>) -> *mut T {
    // Transmute the union
    std::ptr::addr_of_mut!((*entry).value) as _
  }

  /// Helper method to transmute internal pointers.
  ///
  /// # Safety
  ///
  /// For internal use.
  #[inline(always)]
  unsafe fn data_to_entry(data: *mut T) -> *mut RawArenaEntry<T> {
    // Transmute the union
    data as _
  }

  /// Gets the next free entry, allocating if necessary. This is `O(1)` if we have free space in
  /// the arena, `O(?)` if we need to allocate from the allocator (where `?` is defined by the
  /// system allocator).
  ///
  /// # Safety
  ///
  /// As the memory area is considered uninitialized and you must be careful to fully and validly
  /// initialize the underlying data, this method is marked as unsafe.
  ///
  /// This pointer will be invalidated when we drop the `RawArena`, so the allocator API is `unsafe`
  /// as there are no lifetimes here.
  pub unsafe fn allocate(&self) -> *mut T {
    let next = self.next.get();
    let max = self.max.get();

    // Check to see if we have gone past our high-water mark, and we need to extend it. The high-water
    // mark allows us to leave the allocation uninitialized, and assume that the remaining part of the
    // next-free list is a trivial linked-list where each node points to the next one.
    if max == next {
      // Are we out of room?
      if max == self.past_alloc_end {
        // We filled the RawArena, so start allocating
        let new = std::alloc::alloc(Layout::new::<RawArenaEntry<T>>())
          as *mut RawArenaEntry<T>;
        if new.is_null() {
          handle_alloc_error(Layout::new::<RawArenaEntry<T>>());
        }
        return Self::entry_to_data(new);
      }

      // Nope, we can extend by one
      let next = self.max.get().add(1);
      self.next.set(next);
      self.max.set(next);
    } else {
      // We haven't passed the high-water mark, so walk the internal next-free list
      // for our next allocation
      self.next.set((*next).next);
    }

    // Update accounting
    self.allocated.set(self.allocated.get() + 1);
    Self::entry_to_data(next)
  }

  /// Returns the remaining capacity of this [`RawArena`] that can be provided without allocation.
  pub fn remaining(&self) -> usize {
    BASE_CAPACITY - self.allocated.get()
  }

  /// Clear all internally-allocated entries, resetting the arena state to its original state.
  ///
  /// # Safety
  ///
  /// Does not clear system-allocator entries. Pointers previously [`allocate`]d may still be in use.
  pub unsafe fn clear_allocated(&self) {
    self.max.set(self.alloc);
    self.next.set(self.alloc);
    self.allocated.set(0);
  }

  /// Recycle a used item, returning it to the next-free list. Drops the associated item
  /// in place before recycling.
  ///
  /// # Safety
  ///
  /// We assume this pointer is either internal to the arena (in which case we return it
  /// to the arena), or allocated via [`std::alloc::alloc`] in [`allocate`].
  pub unsafe fn recycle(&self, data: *mut T) {
    let entry = Self::data_to_entry(data);
    if entry >= self.alloc && entry < self.past_alloc_end {
      std::ptr::drop_in_place(entry);
      let next = self.next.get();
      self.allocated.set(self.allocated.get() - 1);
      (*entry).next = next;
      self.next.set(entry);
    } else {
      std::ptr::drop_in_place(entry);
      std::alloc::dealloc(entry as _, Layout::new::<RawArenaEntry<T>>());
    }
  }
}

impl<T, const BASE_CAPACITY: usize> Default for RawArena<T, BASE_CAPACITY> {
  /// Allocate an arena, completely initialized. This memory is not zeroed, and
  /// we use the high-water mark to keep track of what we've initialized so far.
  ///
  /// This is safe, because dropping the [`RawArena`] without doing anything to
  /// it is safe.
  fn default() -> Self {
    let alloc =
      unsafe { std::alloc::alloc(Self::layout()) } as *mut RawArenaEntry<T>;
    if alloc.is_null() {
      handle_alloc_error(Self::layout());
    }
    Self {
      alloc,
      past_alloc_end: unsafe { alloc.add(BASE_CAPACITY) },
      max: alloc.into(),
      next: Cell::new(alloc),
      allocated: Default::default(),
    }
  }
}

impl<T, const BASE_CAPACITY: usize> Drop for RawArena<T, BASE_CAPACITY> {
  /// Drop the arena. All pointers are invalidated at this point, except for those
  /// allocated outside outside of the arena.
  ///
  /// The allocation APIs are unsafe because we don't track lifetimes here.
  fn drop(&mut self) {
    unsafe { std::alloc::dealloc(self.alloc as _, Self::layout()) }
  }
}

#[cfg(test)]
mod tests {
  use super::RawArena;

  #[must_use = "If you don't use this, it'll leak!"]
  unsafe fn allocate<const BASE_CAPACITY: usize>(
    arena: &RawArena<usize, BASE_CAPACITY>,
    i: usize,
  ) -> *mut usize {
    let new = arena.allocate();
    *new = i;
    new
  }

  #[test]
  fn test_add_remove_many() {
    let arena = RawArena::<usize, 1024>::default();
    unsafe {
      for i in 0..2000 {
        let v = allocate(&arena, i);
        assert_eq!(arena.remaining(), 1023);
        assert_eq!(*v, i);
        arena.recycle(v);
        assert_eq!(arena.remaining(), 1024);
      }
    }
  }

  #[test]
  fn test_add_clear_many() {
    let arena = RawArena::<usize, 1024>::default();
    unsafe {
      for i in 0..2000 {
        _ = allocate(&arena, i);
        assert_eq!(arena.remaining(), 1023);
        arena.clear_allocated();
        assert_eq!(arena.remaining(), 1024);
      }
    }
  }

  #[test]
  fn test_add_remove_many_separate() {
    let arena = RawArena::<usize, 1024>::default();
    unsafe {
      let mut nodes = vec![];
      // This will spill over into memory allocations
      for i in 0..2000 {
        nodes.push(allocate(&arena, i));
      }
      assert_eq!(arena.remaining(), 0);
      for i in (0..2000).rev() {
        let node = nodes.pop().unwrap();
        assert_eq!(*node, i);
        arena.recycle(node);
      }
      assert_eq!(arena.remaining(), 1024);
    }
  }

  #[test]
  fn test_droppable() {
    // Make sure we correctly drop all the items in this arena if they are droppable
    let arena =
      RawArena::<Box<dyn std::future::Future<Output = ()>>, 16>::default();
    unsafe {
      let mut nodes = vec![];
      // This will spill over into memory allocations
      for _ in 0..20 {
        let node = arena.allocate();
        std::ptr::write(node, Box::new(std::future::pending()));
        nodes.push(node);
      }
      assert_eq!(arena.remaining(), 0);
      for node in nodes {
        arena.recycle(node);
      }
      assert_eq!(arena.remaining(), 16);
    }
  }
}
