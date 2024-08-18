// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

use crate::V8_WRAPPER_OBJECT_INDEX;
use crate::V8_WRAPPER_TYPE_INDEX;

use super::bindings;
use super::snapshot;
use super::snapshot::V8Snapshot;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::Mutex;
use std::sync::Once;

fn v8_init(
  v8_platform: Option<v8::SharedRef<v8::Platform>>,
  snapshot: bool,
  expose_natives: bool,
  import_assertions_enabled: bool,
) {
  #[cfg(feature = "include_icu_data")]
  {
    v8::icu::set_common_data_73(deno_core_icudata::ICU_DATA).unwrap();
  }

  let base_flags = concat!(
    " --wasm-test-streaming",
    " --no-validate-asm",
    " --turbo_fast_api_calls",
    " --harmony-temporal",
    " --js-float16array",
  );
  let snapshot_flags = "--predictable --random-seed=42";
  let expose_natives_flags = "--expose_gc --allow_natives_syntax";
  let lazy_flags = if cfg!(feature = "snapshot_flags_eager_parse") {
    "--no-lazy --no-lazy-eval --no-lazy-streaming"
  } else {
    ""
  };
  let import_assertions_flag = if import_assertions_enabled {
    "--harmony-import-assertions"
  } else {
    "--no-harmony-import-assertions"
  };
  // TODO(bartlomieju): this is ridiculous, rewrite this
  #[allow(clippy::useless_format)]
  let flags = match (snapshot, expose_natives, import_assertions_enabled) {
    (false, false, false) => format!("{base_flags}"),
    (false, false, true) => format!("{base_flags} {import_assertions_flag}"),
    (true, false, false) => {
      format!("{base_flags} {snapshot_flags} {lazy_flags}")
    }
    (true, false, true) => format!(
      "{base_flags} {snapshot_flags} {lazy_flags} {import_assertions_flag}"
    ),
    (false, true, false) => format!("{base_flags} {expose_natives_flags}"),
    (false, true, true) => {
      format!("{base_flags} {expose_natives_flags} {import_assertions_flag}")
    }
    (true, true, false) => {
      format!(
        "{base_flags} {snapshot_flags} {lazy_flags} {expose_natives_flags}"
      )
    }
    (true, true, true) => {
      format!(
        "{base_flags} {snapshot_flags} {lazy_flags} {expose_natives_flags} {import_assertions_flag}"
      )
    }
  };
  v8::V8::set_flags_from_string(&flags);

  let v8_platform = v8_platform.unwrap_or_else(|| {
    if cfg!(any(test, feature = "unsafe_use_unprotected_platform")) {
      // We want to use the unprotected platform for unit tests
      v8::new_unprotected_default_platform(0, false)
    } else {
      v8::new_default_platform(0, false)
    }
    .make_shared()
  });
  v8::V8::initialize_platform(v8_platform.clone());
  v8::V8::initialize();

  v8::cppgc::initalize_process(v8_platform);
}

pub fn init_v8(
  v8_platform: Option<v8::SharedRef<v8::Platform>>,
  snapshot: bool,
  expose_natives: bool,
  import_assertions_enabled: bool,
) {
  static DENO_INIT: Once = Once::new();
  static DENO_SNAPSHOT: AtomicBool = AtomicBool::new(false);
  static DENO_SNAPSHOT_SET: AtomicBool = AtomicBool::new(false);

  if DENO_SNAPSHOT_SET.load(Ordering::SeqCst) {
    let current = DENO_SNAPSHOT.load(Ordering::SeqCst);
    assert_eq!(current, snapshot, "V8 may only be initialized once in either snapshotting or non-snapshotting mode. Either snapshotting or non-snapshotting mode may be used in a single process, not both.");
    DENO_SNAPSHOT_SET.store(true, Ordering::SeqCst);
    DENO_SNAPSHOT.store(snapshot, Ordering::SeqCst);
  }

  DENO_INIT.call_once(move || {
    v8_init(
      v8_platform,
      snapshot,
      expose_natives,
      import_assertions_enabled,
    )
  });
}

fn create_cpp_heap() -> v8::UniqueRef<v8::cppgc::Heap> {
  v8::cppgc::Heap::create(
    v8::V8::get_current_platform(),
    v8::cppgc::HeapCreateParams::default(),
  )
}

pub fn create_isolate(
  will_snapshot: bool,
  maybe_create_params: Option<v8::CreateParams>,
  maybe_startup_snapshot: Option<V8Snapshot>,
  external_refs: &'static v8::ExternalReferences,
) -> v8::OwnedIsolate {
  let mut params = maybe_create_params
    .unwrap_or_default()
    .embedder_wrapper_type_info_offsets(
      V8_WRAPPER_TYPE_INDEX,
      V8_WRAPPER_OBJECT_INDEX,
    )
    .cpp_heap(create_cpp_heap());
  let mut isolate = if will_snapshot {
    snapshot::create_snapshot_creator(
      external_refs,
      maybe_startup_snapshot,
      params,
    )
  } else {
    params = params.external_references(&**external_refs);
    let has_snapshot = maybe_startup_snapshot.is_some();
    if let Some(snapshot) = maybe_startup_snapshot {
      params = params.snapshot_blob(snapshot.0);
    }
    static FIRST_SNAPSHOT_INIT: AtomicBool = AtomicBool::new(false);
    static SNAPSHOW_INIT_MUT: Mutex<()> = Mutex::new(());

    // On Windows, the snapshot deserialization code appears to be crashing and we are not
    // certain of the reason. We take a mutex the first time an isolate with a snapshot to
    // prevent this. https://github.com/denoland/deno/issues/15590
    if cfg!(windows)
      && has_snapshot
      && FIRST_SNAPSHOT_INIT.load(Ordering::SeqCst)
    {
      let _g = SNAPSHOW_INIT_MUT.lock().unwrap();
      let res = v8::Isolate::new(params);
      FIRST_SNAPSHOT_INIT.store(true, Ordering::SeqCst);
      res
    } else {
      v8::Isolate::new(params)
    }
  };

  isolate.set_capture_stack_trace_for_uncaught_exceptions(true, 10);
  isolate.set_promise_reject_callback(bindings::promise_reject_callback);
  isolate.set_prepare_stack_trace_callback(
    crate::error::prepare_stack_trace_callback,
  );
  isolate.set_host_initialize_import_meta_object_callback(
    bindings::host_initialize_import_meta_object_callback,
  );
  isolate.set_host_import_module_dynamically_callback(
    bindings::host_import_module_dynamically_callback,
  );
  isolate.set_wasm_async_resolve_promise_callback(
    bindings::wasm_async_resolve_promise_callback,
  );

  isolate
}
