// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use criterion::*;
use deno_ast::ParseParams;
use deno_core::Extension;
use deno_core::ExtensionFileSourceCode;
use deno_core::JsRuntime;
use deno_core::JsRuntimeForSnapshot;
use deno_core::RuntimeOptions;
use deno_core::Snapshot;
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

fn transpile_extensions(extensions: &mut Vec<Extension>) {
  for extension in extensions {
    for source in extension.esm_files.to_mut() {
      let code = source.load().unwrap().as_str().to_owned();
      let transpiled = deno_ast::parse_module(ParseParams {
        specifier: source.specifier.to_string(),
        text_info: deno_ast::SourceTextInfo::from_string(code),
        media_type: deno_ast::MediaType::TypeScript,
        capture_tokens: false,
        scope_analysis: false,
        maybe_syntax: None,
      })
      .unwrap()
      .transpile(&deno_ast::EmitOptions {
        imports_not_used_as_values: deno_ast::ImportsNotUsedAsValues::Remove,
        inline_source_map: false,
        source_map: true,
        inline_sources: true,
        ..Default::default()
      })
      .unwrap();

      source.code = ExtensionFileSourceCode::Computed(transpiled.text.into());
      source.source_map = Some(transpiled.source_map.unwrap().into_bytes());
    }
  }
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
  fn inner(b: &mut Bencher, transpile: bool) {
    b.iter_custom(|iters| {
      let mut total = 0;
      for _ in 0..iters {
        let mut extensions = make_extensions();
        if transpile {
          transpile_extensions(&mut extensions);
        }
        let runtime = JsRuntimeForSnapshot::new(RuntimeOptions {
          startup_snapshot: None,
          extensions,
          ..Default::default()
        });
        let now = Instant::now();
        runtime.snapshot();
        total += now.elapsed().as_nanos();
      }
      Duration::from_nanos(total as _)
    });
  }

  let mut group = c.benchmark_group("take snapshot");
  group.bench_function("plain", |b| inner(b, false));
  group.bench_function("transpiled", |b| inner(b, true));
  group.finish();
}

fn bench_load_snapshot(c: &mut Criterion) {
  fn inner(b: &mut Bencher, transpile: bool) {
    let mut extensions = make_extensions();
    if transpile {
      transpile_extensions(&mut extensions);
    }
    let runtime = JsRuntimeForSnapshot::new(RuntimeOptions {
      extensions,
      startup_snapshot: None,
      ..Default::default()
    });
    let snapshot = runtime.snapshot().leak();

    b.iter_custom(|iters| {
      let mut total = 0;
      for _ in 0..iters {
        let now = Instant::now();
        let runtime = JsRuntime::new(RuntimeOptions {
          extensions: make_extensions_ops(),
          startup_snapshot: Some(Snapshot::Static(snapshot)),
          ..Default::default()
        });
        total += now.elapsed().as_nanos();
        drop(runtime)
      }
      Duration::from_nanos(total as _)
    });
  }

  let mut group = c.benchmark_group("load snapshot");
  group.bench_function("plain", |b| inner(b, false));
  group.bench_function("transpiled", |b| inner(b, true));
  group.finish();
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
