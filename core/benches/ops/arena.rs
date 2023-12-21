// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use bencher::benchmark_group;
use bencher::benchmark_main;
use bencher::Bencher;
use deno_core::arena::ArenaShared;
use deno_core::arena::ArenaSharedAtomic;
use deno_core::arena::ArenaUnique;
use deno_core::arena::RawArena;
use std::alloc::Layout;
use std::cell::RefCell;
use std::hint::black_box;
use std::rc::Rc;
use std::sync::Arc;

const COUNT: usize = 10_000;

fn bench_arc_arena(b: &mut Bencher) {
  let arena = ArenaSharedAtomic::<RefCell<usize>, COUNT>::default();
  b.iter(|| {
    let mut v = Vec::with_capacity(COUNT);
    for _ in 0..COUNT {
      v.push(arena.allocate(Default::default()));
    }
    for i in v.iter() {
      black_box(i);
    }
    v.clear()
  });
}

fn bench_rc_arena(b: &mut Bencher) {
  let arena = ArenaShared::<RefCell<usize>, COUNT>::default();
  b.iter(|| {
    let mut v = Vec::with_capacity(COUNT);
    for _ in 0..COUNT {
      v.push(arena.allocate(Default::default()));
    }
    for i in v.iter() {
      black_box(i);
    }
    v.clear()
  });
}

fn bench_box_arena(b: &mut Bencher) {
  let arena = ArenaUnique::<RefCell<usize>, COUNT>::default();
  b.iter(|| {
    let mut v = Vec::with_capacity(COUNT);
    for _ in 0..COUNT {
      v.push(arena.allocate(Default::default()));
    }
    for i in v.iter() {
      black_box(i);
    }
    v.clear();
    v
  });
}

#[allow(clippy::arc_with_non_send_sync)]
fn bench_arc(b: &mut Bencher) {
  b.iter(|| {
    let mut v = Vec::with_capacity(COUNT);
    for _ in 0..COUNT {
      v.push(Arc::<RefCell<usize>>::new(Default::default()));
    }
    v = black_box(v);
    v.clear();
    v
  })
}

fn bench_rc(b: &mut Bencher) {
  b.iter(|| {
    let mut v = Vec::with_capacity(COUNT);
    for _ in 0..COUNT {
      v.push(Rc::<RefCell<usize>>::new(Default::default()));
    }
    v = black_box(v);
    v.clear();
    v
  })
}

fn bench_box(b: &mut Bencher) {
  b.iter(|| {
    let mut v = Vec::with_capacity(COUNT);
    for _ in 0..COUNT {
      v.push(Box::<RefCell<usize>>::default());
    }
    v = black_box(v);
    v.clear();
    v
  })
}

fn bench_raw_arena(b: &mut Bencher) {
  let arena = RawArena::<RefCell<usize>, COUNT>::default();
  b.iter(|| {
    let mut v = Vec::with_capacity(COUNT);
    for _ in 0..COUNT {
      unsafe {
        v.push(arena.allocate());
      }
    }
    for i in v.iter() {
      unsafe {
        arena.recycle(*i);
      }
    }
    v = black_box(v);
    v.clear();
    v
  });
}

fn bench_raw_alloc(b: &mut Bencher) {
  b.iter(|| {
    let mut v = Vec::with_capacity(COUNT);
    for _ in 0..COUNT {
      unsafe {
        v.push(std::alloc::alloc(Layout::new::<RefCell<usize>>()));
      }
    }
    for i in v.iter() {
      unsafe {
        std::alloc::dealloc(*i, Layout::new::<RefCell<usize>>());
      }
    }
    v = black_box(v);
    v.clear();
    v
  })
}

benchmark_main!(benches);

benchmark_group!(
  benches,
  bench_arc,
  bench_arc_arena,
  bench_rc,
  bench_rc_arena,
  bench_box,
  bench_box_arena,
  bench_raw_alloc,
  bench_raw_arena,
);
