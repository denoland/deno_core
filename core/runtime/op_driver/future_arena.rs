// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use super::erased_future::ErasedFuture;
use crate::arena::ArenaBox;
use crate::arena::ArenaUnique;
use futures::FutureExt;
use std::future::Future;
use std::pin::Pin;
use std::task::Context;
use std::task::Poll;

const MAX_ARENA_FUTURE_SIZE: usize = 1024;
const FUTURE_ARENA_COUNT: usize = 256;

pub enum FutureAllocation<R: 'static> {
  Arena(ArenaBox<ErasedFuture<MAX_ARENA_FUTURE_SIZE, R>>),
  Box(Pin<Box<dyn Future<Output = R>>>),
}

impl<R> Unpin for FutureAllocation<R> {}

impl<R> Future for FutureAllocation<R> {
  type Output = R;

  #[inline(always)]
  fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
    match self.get_mut() {
      Self::Arena(f) => f.poll_unpin(cx),
      Self::Box(f) => f.poll_unpin(cx),
    }
  }
}

/// An arena of erased futures. Futures too large for the arena, or futures allocated when the
/// arena are full, are automatically moved to the heap instead.
#[repr(transparent)]
pub struct FutureArena {
  arena: ArenaUnique<ErasedFuture<MAX_ARENA_FUTURE_SIZE, ()>>,
}

impl Default for FutureArena {
  fn default() -> Self {
    FutureArena {
      arena: ArenaUnique::with_capacity(FUTURE_ARENA_COUNT),
    }
  }
}

impl FutureArena {
  /// Allocate a future to run in this `FuturesUnordered`. If the future is too large, or the arena
  /// is full, allocated in the heap.
  pub fn allocate<F, R>(&self, f: F) -> FutureAllocation<R>
  where
    F: Future<Output = R> + 'static,
  {
    if std::mem::size_of::<F>() > MAX_ARENA_FUTURE_SIZE {
      FutureAllocation::Box(f.boxed_local())
    } else {
      unsafe {
        match self.arena.reserve_space() {
          Some(reservation) => {
            let alloc = self.arena.complete_reservation(
              reservation,
              std::mem::transmute(
                ErasedFuture::<MAX_ARENA_FUTURE_SIZE, _>::new(f),
              ),
            );
            FutureAllocation::Arena(std::mem::transmute(alloc))
          }
          None => FutureAllocation::Box(f.boxed_local()),
        }
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use std::future::ready;

  use super::*;

  #[test]
  fn test_double_free() {
    let arena = FutureArena::default();
    let f = arena.allocate(async { 1 });
    drop(f);
    let f = arena.allocate(Box::pin(async { 1 }));
    drop(f);
    let f = arena.allocate(ready(Box::new(1)));
    drop(f);
  }

  #[test]
  fn test_exceed_arena() {
    let arena = FutureArena::default();
    let mut v = vec![];
    for _ in 0..1000 {
      v.push(arena.allocate(ready(Box::new(1))));
    }
    drop(v);
  }

  #[test]
  fn test_drop_after_joinset() {
    let arena = FutureArena::default();
    let mut v = vec![];
    for _ in 0..1000 {
      v.push(arena.allocate(ready(Box::new(1))));
    }
    drop(arena);
    drop(v);
  }
}
