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
  predictable: bool,
  expose_natives: bool,
) {
  #[cfg(feature = "include_icu_data")]
  {
    v8::icu::set_common_data_73(deno_core_icudata::ICU_DATA).unwrap();
  }

  let base_flags = concat!(
    " --wasm-test-streaming",
    " --harmony-import-assertions",
    " --harmony-import-attributes",
    " --no-validate-asm",
    " --turbo_fast_api_calls",
    " --harmony-array-from_async",
    " --harmony-iterator-helpers",
    " --harmony-temporal",
  );
  let predictable_flags = "--predictable --random-seed=42";
  let expose_natives_flags = "--expose_gc --allow_natives_syntax";

  #[allow(clippy::useless_format)]
  let flags = match (predictable, expose_natives) {
    (false, false) => format!("{base_flags}"),
    (true, false) => format!("{base_flags} {predictable_flags}"),
    (false, true) => format!("{base_flags} {expose_natives_flags}"),
    (true, true) => {
      format!("{base_flags} {predictable_flags} {expose_natives_flags}")
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
  predictable: bool,
  expose_natives: bool,
) {
  static DENO_INIT: Once = Once::new();
  static DENO_PREDICTABLE: AtomicBool = AtomicBool::new(false);
  static DENO_PREDICTABLE_SET: AtomicBool = AtomicBool::new(false);

  if DENO_PREDICTABLE_SET.load(Ordering::SeqCst) {
    let current = DENO_PREDICTABLE.load(Ordering::SeqCst);
    assert_eq!(current, predictable, "V8 may only be initialized once in either snapshotting or non-snapshotting mode. Either snapshotting or non-snapshotting mode may be used in a single process, not both.");
    DENO_PREDICTABLE_SET.store(true, Ordering::SeqCst);
    DENO_PREDICTABLE.store(predictable, Ordering::SeqCst);
  }

  DENO_INIT
    .call_once(move || v8_init(v8_platform, predictable, expose_natives));
}

pub fn init_cppgc(isolate: &mut v8::Isolate) -> v8::UniqueRef<v8::cppgc::Heap> {
  let heap = v8::cppgc::Heap::create(
    v8::V8::get_current_platform(),
    v8::cppgc::HeapCreateParams::new(v8::cppgc::WrapperDescriptor::new(
      0,
      1,
      crate::cppgc::DEFAULT_CPP_GC_EMBEDDER_ID,
    )),
  );

  isolate.attach_cpp_heap(&heap);
  heap
}

pub fn create_isolate_ptr() -> *mut v8::OwnedIsolate {
  let align = std::mem::align_of::<usize>();
  let layout = std::alloc::Layout::from_size_align(
    std::mem::size_of::<*mut v8::OwnedIsolate>(),
    align,
  )
  .unwrap();
  assert!(layout.size() > 0);
  let isolate_ptr: *mut v8::OwnedIsolate =
    // SAFETY: we just asserted that layout has non-0 size.
    unsafe { std::alloc::alloc(layout) as *mut _ };
  isolate_ptr
}

pub fn create_isolate(
  will_snapshot: bool,
  maybe_create_params: Option<v8::CreateParams>,
  maybe_startup_snapshot: Option<V8Snapshot>,
  external_refs: &'static v8::ExternalReferences,
) -> v8::OwnedIsolate {
  let mut isolate = if will_snapshot {
    snapshot::create_snapshot_creator(external_refs, maybe_startup_snapshot)
  } else {
    let mut params = maybe_create_params
      .unwrap_or_default()
      .embedder_wrapper_type_info_offsets(
        V8_WRAPPER_TYPE_INDEX,
        V8_WRAPPER_OBJECT_INDEX,
      )
      .external_references(&**external_refs);
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
