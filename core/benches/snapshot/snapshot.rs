// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
use criterion::*;
use deno_ast::MediaType;
use deno_ast::ParseParams;
use deno_ast::SourceMapOption;
use deno_ast::SourceTextInfo;
use deno_core::error::AnyError;
use deno_core::Extension;
use deno_core::JsRuntime;
use deno_core::JsRuntimeForSnapshot;
use deno_core::ModuleCodeString;
use deno_core::ModuleName;
use deno_core::RuntimeOptions;
use deno_core::SourceMapData;
use std::rc::Rc;
use std::time::Duration;
use std::time::Instant;
use url::Url;

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

pub fn maybe_transpile_source(
  specifier: ModuleName,
  source: ModuleCodeString,
) -> Result<(ModuleCodeString, Option<SourceMapData>), AnyError> {
  let media_type = MediaType::TypeScript;

  let parsed = deno_ast::parse_module(ParseParams {
    specifier: Url::parse(&specifier).unwrap(),
    text_info: SourceTextInfo::from_string(source.as_str().to_owned()),
    media_type,
    capture_tokens: false,
    scope_analysis: false,
    maybe_syntax: None,
  })?;
  let transpiled_source = parsed.transpile(&deno_ast::EmitOptions {
    imports_not_used_as_values: deno_ast::ImportsNotUsedAsValues::Remove,
    source_map: SourceMapOption::Separate,
    inline_sources: true,
    ..Default::default()
  })?;

  Ok((
    transpiled_source.text.into(),
    transpiled_source.source_map.map(|s| s.into_bytes().into()),
  ))
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
        let extensions = make_extensions();
        let runtime = JsRuntimeForSnapshot::new(RuntimeOptions {
          startup_snapshot: None,
          extension_transpiler: if transpile {
            Some(Rc::new(|specifier, source| {
              maybe_transpile_source(specifier, source)
            }))
          } else {
            None
          },
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
    let extensions = make_extensions();
    let runtime = JsRuntimeForSnapshot::new(RuntimeOptions {
      extensions,
      extension_transpiler: if transpile {
        Some(Rc::new(|specifier, source| {
          maybe_transpile_source(specifier, source)
        }))
      } else {
        None
      },
      startup_snapshot: None,
      ..Default::default()
    });
    let snapshot = runtime.snapshot();
    let snapshot = Box::leak(snapshot);

    b.iter_custom(|iters| {
      let mut total = 0;
      for _ in 0..iters {
        let now = Instant::now();
        let runtime = JsRuntime::new(RuntimeOptions {
          extensions: make_extensions_ops(),
          startup_snapshot: Some(snapshot),
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
