use bencher::*;
use deno_core::error::generic_error;
use deno_core::*;

deno_core::extension!(
  testing,
  ops = [
    op_void,
    op_u32,
    op_option_u32,
    op_string,
    op_string_old,
    op_string_option_u32,
    op_local,
    op_local_scope,
    op_local_nofast,
    op_global,
    op_global_scope,
  ],
  state = |state| {
    state.put(1234u32);
    state.put(10000u16);
  }
);

#[op2(fast)]
pub fn op_void() {}

#[op2(fast)]
pub fn op_u32() -> u32 {
  1
}

#[op2]
pub fn op_option_u32() -> Option<u32> {
  Some(1)
}

#[op2(fast)]
pub fn op_string(#[string] s: &str) -> u32 {
  s.len() as _
}

#[op(fast)]
pub fn op_string_old(s: &str) -> u32 {
  s.len() as _
}

#[op2]
pub fn op_string_option_u32(#[string] s: &str) -> Option<u32> {
  Some(s.len() as _)
}

#[op2(fast)]
pub fn op_local(_s: v8::Local<v8::String>) {}

#[op2]
pub fn op_local_scope(_scope: &mut v8::HandleScope, _s: v8::Local<v8::String>) {
}

#[op2(nofast)]
pub fn op_local_nofast(_s: v8::Local<v8::String>) {}

#[op2]
pub fn op_global(#[global] _s: v8::Global<v8::String>) {}

#[op2]
pub fn op_global_scope(
  _scope: &mut v8::HandleScope,
  #[global] _s: v8::Global<v8::String>,
) {
}

fn bench_op(
  b: &mut Bencher,
  count: usize,
  op: &str,
  arg_count: usize,
  call: &str,
) {
  #[cfg(not(feature = "unsafe_runtime_options"))]
  unreachable!(
    "This benchmark must be run with --features=unsafe_runtime_options"
  );

  let mut runtime = JsRuntime::new(RuntimeOptions {
    extensions: vec![testing::init_ops_and_esm()],
    // We need to feature gate this here to prevent IDE errors
    #[cfg(feature = "unsafe_runtime_options")]
    unsafe_expose_natives_and_gc: true,
    ..Default::default()
  });
  let err_mapper =
    |err| generic_error(format!("{op} test failed ({call}): {err:?}"));

  let args = (0..arg_count)
    .map(|n| format!("arg{n}"))
    .collect::<Vec<_>>()
    .join(", ");

  // Prime the optimizer
  runtime
    .execute_script(
      "",
      FastString::Owned(
        format!(
          r"
const LARGE_STRING_1000000 = '*'.repeat(1000000);
const LARGE_STRING_1000 = '*'.repeat(1000);
const LARGE_STRING_UTF8_1000000 = '\u1000'.repeat(1000000);
const LARGE_STRING_UTF8_1000 = '\u1000'.repeat(1000);
const {{ {op}: op }} = Deno.core.ensureFastOps();
function {op}({args}) {{
  op({args});
}}
let accum = 0;
let __index__ = 0;
%PrepareFunctionForOptimization({op});
{call};
%OptimizeFunctionOnNextCall({op});
{call};

function bench() {{
  let accum = 0;
  for (let __index__ = 0; __index__ < {count}; __index__++) {{ {call} }}
  return accum;
}}
%PrepareFunctionForOptimization(bench);
bench();
%OptimizeFunctionOnNextCall(bench);
bench();
        "
        )
        .into(),
      ),
    )
    .map_err(err_mapper)
    .unwrap();
  let bench = runtime.execute_script("", ascii_str!("bench")).unwrap();
  let mut scope = runtime.handle_scope();
  let bench: v8::Local<v8::Function> =
    v8::Local::new(&mut scope, bench).try_into().unwrap();
  b.iter(|| {
    let recv = v8::undefined(&mut scope).try_into().unwrap();
    bench.call(&mut scope, recv, &[]);
  });
}

const BENCH_COUNT: usize = 1000;
const LARGE_BENCH_COUNT: usize = 10;

/// Tests the overhead of execute_script.
fn baseline(b: &mut Bencher) {
  bench_op(b, BENCH_COUNT, "op_void", 0, "accum += __index__;");
}

/// A void function with no return value.
fn bench_op_void(b: &mut Bencher) {
  bench_op(b, BENCH_COUNT, "op_void", 0, "op_void()");
}

/// A function with a numeric return value.
fn bench_op_u32(b: &mut Bencher) {
  bench_op(b, BENCH_COUNT, "op_u32", 0, "accum += op_u32();");
}

/// A function with an optional return value (making it non-fast).
fn bench_op_option_u32(b: &mut Bencher) {
  bench_op(
    b,
    BENCH_COUNT,
    "op_option_u32",
    0,
    "accum += op_option_u32();",
  );
}

/// A string function with a numeric return value.
fn bench_op_string(b: &mut Bencher) {
  bench_op(b, BENCH_COUNT, "op_string", 1, "accum += op_string('this is a reasonably long string that we would like to get the length of!');");
}

/// A string function with a numeric return value.
fn bench_op_string_large_1000(b: &mut Bencher) {
  bench_op(
    b,
    BENCH_COUNT,
    "op_string",
    1,
    "accum += op_string(LARGE_STRING_1000);",
  );
}

/// A string function with a numeric return value.
fn bench_op_string_large_1000000(b: &mut Bencher) {
  bench_op(
    b,
    LARGE_BENCH_COUNT,
    "op_string",
    1,
    "accum += op_string(LARGE_STRING_1000000);",
  );
}

/// A string function with a numeric return value.
fn bench_op_string_large_utf8_1000(b: &mut Bencher) {
  bench_op(
    b,
    BENCH_COUNT,
    "op_string",
    1,
    "accum += op_string(LARGE_STRING_UTF8_1000);",
  );
}

/// A string function with a numeric return value.
fn bench_op_string_large_utf8_1000000(b: &mut Bencher) {
  bench_op(
    b,
    LARGE_BENCH_COUNT,
    "op_string",
    1,
    "accum += op_string(LARGE_STRING_UTF8_1000000);",
  );
}

/// A string function with a numeric return value.
fn bench_op_string_old(b: &mut Bencher) {
  bench_op(b, BENCH_COUNT, "op_string_old", 1, "accum += op_string_old('this is a reasonably long string that we would like to get the length of!');");
}

/// A string function with a numeric return value.
fn bench_op_string_old_large_1000(b: &mut Bencher) {
  bench_op(
    b,
    BENCH_COUNT,
    "op_string_old",
    1,
    "accum += op_string_old(LARGE_STRING_1000);",
  );
}

/// A string function with a numeric return value.
fn bench_op_string_old_large_1000000(b: &mut Bencher) {
  bench_op(
    b,
    LARGE_BENCH_COUNT,
    "op_string_old",
    1,
    "accum += op_string_old(LARGE_STRING_1000000);",
  );
}

/// A string function with a numeric return value.
fn bench_op_string_old_large_utf8_1000(b: &mut Bencher) {
  bench_op(
    b,
    BENCH_COUNT,
    "op_string_old",
    1,
    "accum += op_string_old(LARGE_STRING_UTF8_1000);",
  );
}

/// A string function with a numeric return value.
fn bench_op_string_old_large_utf8_1000000(b: &mut Bencher) {
  bench_op(
    b,
    LARGE_BENCH_COUNT,
    "op_string_old",
    1,
    "accum += op_string_old(LARGE_STRING_UTF8_1000000);",
  );
}

/// A string function with an option numeric return value.
fn bench_op_string_option_u32(b: &mut Bencher) {
  bench_op(b, BENCH_COUNT, "op_string_option_u32", 1, "accum += op_string_option_u32('this is a reasonably long string that we would like to get the length of!');");
}

/// A fast function that takes a v8::Local<String>
fn bench_op_v8_local(b: &mut Bencher) {
  bench_op(b, BENCH_COUNT, "op_local", 1, "op_local('this is a reasonably long string that we would like to get the length of!');");
}

/// A function that takes a v8::Local<String>
fn bench_op_v8_local_scope(b: &mut Bencher) {
  bench_op(b, BENCH_COUNT, "op_local_scope", 1, "op_local_scope('this is a reasonably long string that we would like to get the length of!');");
}

/// A function that takes a v8::Local<String>
fn bench_op_v8_local_nofast(b: &mut Bencher) {
  bench_op(b, BENCH_COUNT, "op_local_nofast", 1, "op_local_nofast('this is a reasonably long string that we would like to get the length of!');");
}

/// A function that takes a v8::Global<String>
fn bench_op_v8_global(b: &mut Bencher) {
  bench_op(b, BENCH_COUNT, "op_global", 1, "op_global('this is a reasonably long string that we would like to get the length of!');");
}

/// A function that takes a v8::Global<String>
fn bench_op_v8_global_scope(b: &mut Bencher) {
  bench_op(b, BENCH_COUNT, "op_global_scope", 1, "op_global_scope('this is a reasonably long string that we would like to get the length of!');");
}

benchmark_group!(
  benches,
  baseline,
  bench_op_void,
  bench_op_u32,
  bench_op_option_u32,
  bench_op_string,
  bench_op_string_large_1000,
  bench_op_string_large_1000000,
  bench_op_string_large_utf8_1000,
  bench_op_string_large_utf8_1000000,
  bench_op_string_old,
  bench_op_string_old_large_1000,
  bench_op_string_old_large_1000000,
  bench_op_string_old_large_utf8_1000,
  bench_op_string_old_large_utf8_1000000,
  bench_op_string_option_u32,
  bench_op_v8_local,
  bench_op_v8_local_scope,
  bench_op_v8_local_nofast,
  bench_op_v8_global,
  bench_op_v8_global_scope,
);

benchmark_main!(benches);
