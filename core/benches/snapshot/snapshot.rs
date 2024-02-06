// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use criterion::*;
use deno_core::Extension;
use deno_core::JsRuntime;
use deno_core::JsRuntimeForSnapshot;
use deno_core::RuntimeOptions;
use deno_core::Snapshot;
use std::ops::Deref;
use std::time::Duration;
use std::time::Instant;

macro_rules! fake_extensions {
  ($which:ident, $($name:ident),+) => (
    {
      $(
        mod $name {
          deno_core::extension!(
            $name,
            ops = [ ops::$name ],
            esm_entry_point = concat!("ext:", stringify!($name), "/file.js"),
            esm = [ dir "benches/snapshot", "file.js", "file2.js" ]
          );

          mod ops {
            #[deno_core::op2(fast)]
            pub fn $name() {
            }
          }
        }
      )+

      vec![$($name::$name::$which()),+]
    }
  );
}

fn make_extensions() -> Vec<Extension> {
  fake_extensions!(init_ops_and_esm, a, b, c, d, e, f, g, h, i, j, k, l, m, n)
}

fn make_extensions_ops() -> Vec<Extension> {
  fake_extensions!(init_ops, a, b, c, d, e, f, g, h, i, j, k, l, m, n)
}

fn bench_take_snapshot_empty(c: &mut Criterion) {
  c.bench_function("take snapshot (empty)", |b| {
    b.iter_custom(|iters| {
      let mut total = 0;
      for _ in 0..iters {
        let runtime = JsRuntimeForSnapshot::new(RuntimeOptions {
          startup_snapshot: None,
          ..Default::default()
        });
        let now = Instant::now();
        runtime.snapshot();
        total += now.elapsed().as_nanos();
      }
      Duration::from_nanos(total as _)
    });
  });
}

fn bench_take_snapshot(c: &mut Criterion) {
  c.bench_function("take snapshot", |b| {
    b.iter_custom(|iters| {
      let mut total = 0;
      for _ in 0..iters {
        let runtime = JsRuntimeForSnapshot::new(RuntimeOptions {
          startup_snapshot: None,
          extensions: make_extensions(),
          ..Default::default()
        });
        let now = Instant::now();
        runtime.snapshot();
        total += now.elapsed().as_nanos();
      }
      Duration::from_nanos(total as _)
    });
  });
}

fn bench_load_snapshot(c: &mut Criterion) {
  c.bench_function("load snapshot", |b| {
    let runtime = JsRuntimeForSnapshot::new(RuntimeOptions {
      extensions: make_extensions(),
      startup_snapshot: None,
      ..Default::default()
    });
    let snapshot = runtime.snapshot();
    let snapshot_slice =
      Box::leak(snapshot.deref().to_vec().into_boxed_slice());

    b.iter_custom(|iters| {
      let mut total = 0;
      for _ in 0..iters {
        let now = Instant::now();
        let runtime = JsRuntime::new(RuntimeOptions {
          extensions: make_extensions_ops(),
          startup_snapshot: Some(Snapshot::Static(snapshot_slice)),
          ..Default::default()
        });
        total += now.elapsed().as_nanos();
        drop(runtime)
      }
      Duration::from_nanos(total as _)
    });
  });
}

criterion_group!(
  name = benches;
  config = Criterion::default().sample_size(50);
  targets =
    bench_take_snapshot_empty,
    bench_take_snapshot,
    bench_load_snapshot,
);

criterion_main!(benches);
