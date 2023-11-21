// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.

use super::bindings;
use super::jsrealm::JsRealmInner;
use super::snapshot_util;
use crate::error::exception_to_err_result;
use crate::error::generic_error;
use crate::error::GetErrorClassFn;
use crate::error::JsError;
use crate::extensions::EventLoopMiddlewareFn;
use crate::extensions::GlobalObjectMiddlewareFn;
use crate::extensions::GlobalTemplateMiddlewareFn;
use crate::extensions::OpDecl;
use crate::include_js_files;
use crate::inspector::JsRuntimeInspector;
use crate::module_specifier::ModuleSpecifier;
use crate::modules::AssertedModuleType;
use crate::modules::ExtModuleLoader;
use crate::modules::ModuleCode;
use crate::modules::ModuleId;
use crate::modules::ModuleLoader;
use crate::modules::ModuleMap;
use crate::modules::ValidateImportAttributesCb;
use crate::ops::*;
use crate::ops_metrics::dispatch_metrics_async;
use crate::ops_metrics::OpMetricsEvent;
use crate::ops_metrics::OpMetricsFactoryFn;
use crate::runtime::ContextState;
use crate::runtime::JsRealm;
use crate::source_map::SourceMapCache;
use crate::source_map::SourceMapGetter;
use crate::Extension;
use crate::ExtensionFileSource;
use crate::FeatureChecker;
use crate::NoopModuleLoader;
use crate::OpMiddlewareFn;
use crate::OpResult;
use crate::OpState;
use crate::V8_WRAPPER_OBJECT_INDEX;
use crate::V8_WRAPPER_TYPE_INDEX;
use anyhow::Context as AnyhowContext;
use anyhow::Error;
use futures::channel::oneshot;
use futures::future::poll_fn;
use futures::Future;
use smallvec::SmallVec;
use std::any::Any;
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::mem::ManuallyDrop;
use std::ops::Deref;
use std::ops::DerefMut;
use std::option::Option;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::Once;
use std::task::Context;
use std::task::Poll;
use v8::Isolate;

const STATE_DATA_OFFSET: u32 = 0;

pub enum Snapshot {
  Static(&'static [u8]),
  JustCreated(v8::StartupData),
  Boxed(Box<[u8]>),
}

/// Objects that need to live as long as the isolate
#[derive(Default)]
pub(crate) struct IsolateAllocations {
  pub(crate) near_heap_limit_callback_data:
    Option<(Box<RefCell<dyn Any>>, v8::NearHeapLimitCallback)>,
}

/// ManuallyDrop<Rc<...>> is clone, but it returns a ManuallyDrop<Rc<...>> which is a massive
/// memory-leak footgun.
pub(crate) struct ManuallyDropRc<T>(ManuallyDrop<Rc<T>>);

impl<T> ManuallyDropRc<T> {
  pub fn clone(&self) -> Rc<T> {
    self.0.deref().clone()
  }
}

impl<T> Deref for ManuallyDropRc<T> {
  type Target = Rc<T>;
  fn deref(&self) -> &Self::Target {
    self.0.deref()
  }
}

impl<T> DerefMut for ManuallyDropRc<T> {
  fn deref_mut(&mut self) -> &mut Self::Target {
    self.0.deref_mut()
  }
}

/// This struct contains the [`JsRuntimeState`] and [`v8::OwnedIsolate`] that are required
/// to do an orderly shutdown of V8. We keep these in a separate struct to allow us to control
/// the destruction more closely, as snapshots require the isolate to be destroyed by the
/// snapshot process, not the destructor.
///
/// The way rusty_v8 works w/snapshots is that the [`v8::OwnedIsolate`] gets consumed by a
/// [`v8::snapshot::SnapshotCreator`] that is stored in its annex. It's a bit awkward, because this
/// means we cannot let it drop (because we don't have it after a snapshot). On top of that, we have
/// to consume it in the snapshot creator because otherwise it panics.
///
/// This inner struct allows us to let the outer JsRuntime drop normally without a Drop impl, while we
/// control dropping more closely here using ManuallyDrop.
pub(crate) struct InnerIsolateState {
  will_snapshot: bool,
  main_realm: Option<JsRealm>,
  pub(crate) state: ManuallyDropRc<RefCell<JsRuntimeState>>,
  v8_isolate: ManuallyDrop<v8::OwnedIsolate>,
}

impl InnerIsolateState {
  /// Clean out the opstate and take the inspector to prevent the inspector from getting destroyed
  /// after we've torn down the contexts. If the inspector is not correctly torn down, random crashes
  /// happen in tests (and possibly for users using the inspector).
  pub fn prepare_for_cleanup(&mut self) {
    let mut state = self.state.borrow_mut();
    let inspector = state.inspector.take();
    state.op_state.borrow_mut().clear();
    if let Some(inspector) = inspector {
      assert_eq!(
        Rc::strong_count(&inspector),
        1,
        "The inspector must be dropped before the runtime"
      );
    }
  }

  pub fn cleanup(&mut self) {
    self.prepare_for_cleanup();

    let state_ptr = self.v8_isolate.get_data(STATE_DATA_OFFSET);
    // SAFETY: We are sure that it's a valid pointer for whole lifetime of
    // the runtime.
    _ = unsafe { Rc::from_raw(state_ptr as *const RefCell<JsRuntimeState>) };

    self.main_realm.take().unwrap().0.destroy();

    debug_assert_eq!(Rc::strong_count(&self.state), 1);
  }

  pub fn prepare_for_snapshot(mut self) -> v8::OwnedIsolate {
    self.cleanup();
    // SAFETY: We're copying out of self and then immediately forgetting self
    let (state, isolate) = unsafe {
      (
        ManuallyDrop::take(&mut self.state.0),
        ManuallyDrop::take(&mut self.v8_isolate),
      )
    };
    std::mem::forget(self);
    drop(state);
    isolate
  }
}

impl Drop for InnerIsolateState {
  fn drop(&mut self) {
    self.cleanup();
    // SAFETY: We gotta drop these
    unsafe {
      ManuallyDrop::drop(&mut self.state.0);
      if self.will_snapshot {
        // Create the snapshot and just drop it.
        eprintln!("WARNING: v8::OwnedIsolate for snapshot was leaked");
      } else {
        ManuallyDrop::drop(&mut self.v8_isolate);
      }
    }
  }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum InitMode {
  /// We have no snapshot -- this is a pristine context.
  New,
  /// We are using a snapshot, thus certain initialization steps are skipped.
  FromSnapshot {
    // Can we skip the work of op registration?
    skip_op_registration: bool,
  },
}

impl InitMode {
  fn from_options(options: &RuntimeOptions) -> Self {
    match options.startup_snapshot {
      None => Self::New,
      Some(_) => Self::FromSnapshot {
        skip_op_registration: options.skip_op_registration,
      },
    }
  }

  #[inline]
  pub fn is_from_snapshot(&self) -> bool {
    matches!(self, Self::FromSnapshot { .. })
  }
}

pub(crate) const BUILTIN_SOURCES: [ExtensionFileSource; 3] = include_js_files!(
  core
  "00_primordials.js",
  "01_core.js",
  "02_error.js",
);

/// A single execution context of JavaScript. Corresponds roughly to the "Web
/// Worker" concept in the DOM.
////
/// The JsRuntime future completes when there is an error or when all
/// pending ops have completed.
///
/// Use [`JsRuntimeForSnapshot`] to be able to create a snapshot.
///
/// Note: since V8 11.6, all runtimes must have a common parent thread that
/// initalized the V8 platform. This can be done by calling
/// [`JsRuntime::init_platform`] explicitly, or it will be done automatically on
/// the calling thread when the first runtime is created.
pub struct JsRuntime {
  pub(crate) inner: InnerIsolateState,
  pub(crate) allocations: IsolateAllocations,
  extensions: Vec<Extension>,
  event_loop_middlewares: Vec<EventLoopMiddlewareFn>,
  init_mode: InitMode,
  // Marks if this is considered the top-level runtime. Used only by inspector.
  is_main_runtime: bool,
}

/// The runtime type used for snapshot creation.
pub struct JsRuntimeForSnapshot(JsRuntime);

impl Deref for JsRuntimeForSnapshot {
  type Target = JsRuntime;

  fn deref(&self) -> &Self::Target {
    &self.0
  }
}

impl DerefMut for JsRuntimeForSnapshot {
  fn deref_mut(&mut self) -> &mut Self::Target {
    &mut self.0
  }
}

pub struct CrossIsolateStore<T>(Arc<Mutex<CrossIsolateStoreInner<T>>>);

struct CrossIsolateStoreInner<T> {
  map: HashMap<u32, T>,
  last_id: u32,
}

impl<T> CrossIsolateStore<T> {
  pub(crate) fn insert(&self, value: T) -> u32 {
    let mut store = self.0.lock().unwrap();
    let last_id = store.last_id;
    store.map.insert(last_id, value);
    store.last_id += 1;
    last_id
  }

  pub(crate) fn take(&self, id: u32) -> Option<T> {
    let mut store = self.0.lock().unwrap();
    store.map.remove(&id)
  }
}

impl<T> Default for CrossIsolateStore<T> {
  fn default() -> Self {
    CrossIsolateStore(Arc::new(Mutex::new(CrossIsolateStoreInner {
      map: Default::default(),
      last_id: 0,
    })))
  }
}

impl<T> Clone for CrossIsolateStore<T> {
  fn clone(&self) -> Self {
    Self(self.0.clone())
  }
}

pub type SharedArrayBufferStore =
  CrossIsolateStore<v8::SharedRef<v8::BackingStore>>;

pub type CompiledWasmModuleStore = CrossIsolateStore<v8::CompiledWasmModule>;

/// Internal state for JsRuntime which is stored in one of v8::Isolate's
/// embedder slots.
pub struct JsRuntimeState {
  pub(crate) has_tick_scheduled: bool,
  pub(crate) source_map_getter: Option<Rc<Box<dyn SourceMapGetter>>>,
  pub(crate) source_map_cache: Rc<RefCell<SourceMapCache>>,
  pub(crate) op_state: Rc<RefCell<OpState>>,
  pub(crate) shared_array_buffer_store: Option<SharedArrayBufferStore>,
  pub(crate) compiled_wasm_module_store: Option<CompiledWasmModuleStore>,
  /// The error that was passed to an `op_dispatch_exception` call.
  /// It will be retrieved by `exception_to_err_result` and used as an error
  /// instead of any other exceptions.
  // TODO(nayeemrmn): This is polled in `exception_to_err_result()` which is
  // flimsy. Try to poll it similarly to `pending_promise_rejections`.
  pub(crate) dispatched_exception: Option<v8::Global<v8::Value>>,
  pub(crate) inspector: Option<Rc<RefCell<JsRuntimeInspector>>>,
  pub(crate) validate_import_attributes_cb: ValidateImportAttributesCb,
}

impl JsRuntimeState {}

fn v8_init(
  v8_platform: Option<v8::SharedRef<v8::Platform>>,
  predictable: bool,
  expose_natives: bool,
) {
  #[cfg(feature = "include_icu_data")]
  {
    // Include 10MB ICU data file.
    #[repr(C, align(16))]
    struct IcuData([u8; 10631872]);
    static ICU_DATA: IcuData = IcuData(*include_bytes!("icudtl.dat"));
    v8::icu::set_common_data_73(&ICU_DATA.0).unwrap();
  }

  let base_flags = concat!(
    " --wasm-test-streaming",
    " --harmony-import-assertions",
    " --harmony-import-attributes",
    " --no-validate-asm",
    " --turbo_fast_api_calls",
    " --harmony-change-array-by-copy",
    " --harmony-array-from_async",
    " --harmony-iterator-helpers",
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

  let v8_platform = v8_platform
    .unwrap_or_else(|| v8::new_default_platform(0, false).make_shared());
  v8::V8::initialize_platform(v8_platform);
  v8::V8::initialize();
}

#[derive(Default)]
pub struct RuntimeOptions {
  /// Source map reference for errors.
  pub source_map_getter: Option<Box<dyn SourceMapGetter>>,

  /// Allows to map error type to a string "class" used to represent
  /// error in JavaScript.
  pub get_error_class_fn: Option<GetErrorClassFn>,

  /// Implementation of `ModuleLoader` which will be
  /// called when V8 requests to load ES modules in the main realm.
  ///
  /// If not provided runtime will error if code being
  /// executed tries to load modules.
  pub module_loader: Option<Rc<dyn ModuleLoader>>,

  /// Provide a function that may optionally provide a metrics collector
  /// for a given op.
  pub op_metrics_factory_fn: Option<OpMetricsFactoryFn>,

  /// JsRuntime extensions, not to be confused with ES modules.
  /// Only ops registered by extensions will be initialized. If you need
  /// to execute JS code from extensions, pass source files in `js` or `esm`
  /// option on `ExtensionBuilder`.
  ///
  /// If you are creating a runtime from a snapshot take care not to include
  /// JavaScript sources in the extensions.
  pub extensions: Vec<Extension>,

  /// If provided, the module map will be cleared and left only with the specifiers
  /// in this list. If not provided, the module map is left intact.
  pub preserve_snapshotted_modules: Option<&'static [&'static str]>,

  /// V8 snapshot that should be loaded on startup.
  pub startup_snapshot: Option<Snapshot>,

  /// Should op registration be skipped?
  pub skip_op_registration: bool,

  /// Isolate creation parameters.
  pub create_params: Option<v8::CreateParams>,

  /// V8 platform instance to use. Used when Deno initializes V8
  /// (which it only does once), otherwise it's silently dropped.
  pub v8_platform: Option<v8::SharedRef<v8::Platform>>,

  /// The store to use for transferring SharedArrayBuffers between isolates.
  /// If multiple isolates should have the possibility of sharing
  /// SharedArrayBuffers, they should use the same [SharedArrayBufferStore]. If
  /// no [SharedArrayBufferStore] is specified, SharedArrayBuffer can not be
  /// serialized.
  pub shared_array_buffer_store: Option<SharedArrayBufferStore>,

  /// The store to use for transferring `WebAssembly.Module` objects between
  /// isolates.
  /// If multiple isolates should have the possibility of sharing
  /// `WebAssembly.Module` objects, they should use the same
  /// [CompiledWasmModuleStore]. If no [CompiledWasmModuleStore] is specified,
  /// `WebAssembly.Module` objects cannot be serialized.
  pub compiled_wasm_module_store: Option<CompiledWasmModuleStore>,

  /// Start inspector instance to allow debuggers to connect.
  pub inspector: bool,

  /// Describe if this is the main runtime instance, used by debuggers in some
  /// situation - like disconnecting when program finishes running.
  pub is_main: bool,

  #[cfg(any(test, feature = "unsafe_runtime_options"))]
  /// Should this isolate expose the v8 natives (eg: %OptimizeFunctionOnNextCall) and
  /// GC control functions (`gc()`)? WARNING: This should not be used for production code as
  /// this may expose the runtime to security vulnerabilities.
  pub unsafe_expose_natives_and_gc: bool,

  /// An optional instance of `FeatureChecker`. If one is not provided, the
  /// default instance will be created that has no features enabled.
  pub feature_checker: Option<Arc<FeatureChecker>>,

  /// A callback that can be used to validate import attributes received at
  /// the import site. If not callback is provided, a default one is used. The
  /// default callback only allows `"type"` attribute, with a value of `"json"`.
  pub validate_import_attributes_cb: Option<ValidateImportAttributesCb>,
}

impl RuntimeOptions {
  #[cfg(any(test, feature = "unsafe_runtime_options"))]
  fn unsafe_expose_natives_and_gc(&self) -> bool {
    self.unsafe_expose_natives_and_gc
  }

  #[cfg(not(any(test, feature = "unsafe_runtime_options")))]
  fn unsafe_expose_natives_and_gc(&self) -> bool {
    false
  }
}

#[derive(Copy, Clone, Debug)]
pub struct PollEventLoopOptions {
  pub wait_for_inspector: bool,
  pub pump_v8_message_loop: bool,
}

impl Default for PollEventLoopOptions {
  fn default() -> Self {
    Self {
      wait_for_inspector: true,
      pump_v8_message_loop: true,
    }
  }
}

#[derive(Default)]
pub struct CreateRealmOptions {
  /// Implementation of `ModuleLoader` which will be
  /// called when V8 requests to load ES modules in the realm.
  ///
  /// If not provided, there will be an error if code being
  /// executed tries to load modules from the realm.
  pub module_loader: Option<Rc<dyn ModuleLoader>>,
}

impl JsRuntime {
  /// Explicitly initalizes the V8 platform using the passed platform. This
  /// should only be called once per process. Further calls will be silently
  /// ignored.
  #[cfg(not(any(test, feature = "unsafe_runtime_options")))]
  pub fn init_platform(v8_platform: Option<v8::SharedRef<v8::Platform>>) {
    JsRuntime::init_v8(v8_platform, cfg!(test), false);
  }

  /// Explicitly initalizes the V8 platform using the passed platform. This
  /// should only be called once per process. Further calls will be silently
  /// ignored.
  ///
  /// The `expose_natives` flag is used to expose the v8 natives
  /// (eg: %OptimizeFunctionOnNextCall) and GC control functions (`gc()`).
  /// WARNING: This should not be used for production code as
  /// this may expose the runtime to security vulnerabilities.
  #[cfg(any(test, feature = "unsafe_runtime_options"))]
  pub fn init_platform(
    v8_platform: Option<v8::SharedRef<v8::Platform>>,
    expose_natives: bool,
  ) {
    JsRuntime::init_v8(v8_platform, cfg!(test), expose_natives);
  }

  /// Only constructor, configuration is done through `options`.
  pub fn new(mut options: RuntimeOptions) -> JsRuntime {
    JsRuntime::init_v8(
      options.v8_platform.take(),
      cfg!(test),
      options.unsafe_expose_natives_and_gc(),
    );
    JsRuntime::new_inner(options, false)
  }

  pub(crate) fn state_from(
    isolate: &v8::Isolate,
  ) -> Rc<RefCell<JsRuntimeState>> {
    let state_ptr = isolate.get_data(STATE_DATA_OFFSET);
    let state_rc =
      // SAFETY: We are sure that it's a valid pointer for whole lifetime of
      // the runtime.
      unsafe { Rc::from_raw(state_ptr as *const RefCell<JsRuntimeState>) };
    let state = state_rc.clone();
    std::mem::forget(state_rc);
    state
  }

  /// Returns the `OpState` associated with the passed `Isolate`.
  pub fn op_state_from(isolate: &v8::Isolate) -> Rc<RefCell<OpState>> {
    let state = Self::state_from(isolate);
    let state = state.borrow();
    state.op_state.clone()
  }

  pub(crate) fn has_more_work(scope: &mut v8::HandleScope) -> bool {
    EventLoopPendingState::new_from_scope(scope).is_pending()
  }

  fn init_v8(
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

  fn new_inner(mut options: RuntimeOptions, will_snapshot: bool) -> JsRuntime {
    let init_mode = InitMode::from_options(&options);
    let (op_state, ops) = Self::create_opstate(&mut options);
    let op_state = Rc::new(RefCell::new(op_state));

    // Collect event-loop middleware, global template middleware, global object
    // middleware, and additional ExternalReferences from extensions.
    let mut event_loop_middlewares =
      Vec::with_capacity(options.extensions.len());
    let mut global_template_middlewares =
      Vec::with_capacity(options.extensions.len());
    let mut global_object_middlewares =
      Vec::with_capacity(options.extensions.len());
    let mut additional_references =
      Vec::with_capacity(options.extensions.len());
    for extension in &mut options.extensions {
      if let Some(middleware) = extension.get_event_loop_middleware() {
        event_loop_middlewares.push(middleware);
      }
      if let Some(middleware) = extension.get_global_template_middleware() {
        global_template_middlewares.push(middleware);
      }
      if let Some(middleware) = extension.get_global_object_middleware() {
        global_object_middlewares.push(middleware);
      }
      additional_references
        .extend_from_slice(extension.get_external_references());
    }

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

    let validate_import_attributes_cb = options
      .validate_import_attributes_cb
      .unwrap_or_else(|| Box::new(crate::modules::validate_import_attributes));

    let state_rc = Rc::new(RefCell::new(JsRuntimeState {
      has_tick_scheduled: false,
      source_map_getter: options.source_map_getter.map(Rc::new),
      source_map_cache: Default::default(),
      shared_array_buffer_store: options.shared_array_buffer_store,
      compiled_wasm_module_store: options.compiled_wasm_module_store,
      op_state: op_state.clone(),
      dispatched_exception: None,
      // Some fields are initialized later after isolate is created
      inspector: None,
      validate_import_attributes_cb,
    }));

    let weak = Rc::downgrade(&state_rc);
    let context_state = Rc::new(RefCell::new(ContextState::default()));
    let count = ops.len();
    let mut op_ctxs = ops
      .into_iter()
      .enumerate()
      .map(|(id, decl)| {
        let metrics_fn = options
          .op_metrics_factory_fn
          .as_ref()
          .and_then(|f| (f)(id as _, count, &decl));
        OpCtx::new(
          id as _,
          std::ptr::null_mut(),
          context_state.clone(),
          Rc::new(decl),
          op_state.clone(),
          weak.clone(),
          options.get_error_class_fn.unwrap_or(&|_| "Error"),
          metrics_fn,
        )
      })
      .collect::<Vec<_>>()
      .into_boxed_slice();
    context_state.borrow_mut().isolate = Some(isolate_ptr);

    let refs = bindings::external_references(&op_ctxs, &additional_references);
    // V8 takes ownership of external_references.
    let refs: &'static v8::ExternalReferences = Box::leak(Box::new(refs));

    let mut isolate = if will_snapshot {
      snapshot_util::create_snapshot_creator(
        refs,
        options.startup_snapshot.take(),
      )
    } else {
      let mut params = options
        .create_params
        .take()
        .unwrap_or_default()
        .embedder_wrapper_type_info_offsets(
          V8_WRAPPER_TYPE_INDEX,
          V8_WRAPPER_OBJECT_INDEX,
        )
        .external_references(&**refs);
      if let Some(snapshot) = options.startup_snapshot.take() {
        params = match snapshot {
          Snapshot::Static(data) => params.snapshot_blob(data),
          Snapshot::JustCreated(data) => params.snapshot_blob(data),
          Snapshot::Boxed(data) => params.snapshot_blob(data),
        };
      }
      v8::Isolate::new(params)
    };

    for op_ctx in op_ctxs.iter_mut() {
      op_ctx.isolate = isolate.as_mut() as *mut Isolate;
    }
    context_state.borrow_mut().op_ctxs = op_ctxs;

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

    let (main_context, snapshotted_data) = {
      let scope = &mut v8::HandleScope::new(&mut isolate);

      let context = create_context(
        scope,
        &global_template_middlewares,
        &global_object_middlewares,
      );

      // Get module map data from the snapshot
      let snapshotted_data = if init_mode.is_from_snapshot() {
        Some(snapshot_util::get_snapshotted_data(scope, context))
      } else {
        None
      };

      (v8::Global::new(scope, context), snapshotted_data)
    };

    // SAFETY: this is first use of `isolate_ptr` so we are sure we're
    // not overwriting an existing pointer.
    isolate = unsafe {
      isolate_ptr.write(isolate);
      isolate_ptr.read()
    };

    let mut context_scope: v8::HandleScope =
      v8::HandleScope::with_context(&mut isolate, &main_context);
    let scope = &mut context_scope;
    let context = v8::Local::new(scope, &main_context);

    bindings::initialize_context(
      scope,
      context,
      &context_state.borrow().op_ctxs,
      init_mode,
    );

    context.set_slot(scope, context_state.clone());

    op_state.borrow_mut().put(isolate_ptr);
    let inspector = if options.inspector {
      Some(JsRuntimeInspector::new(scope, context, options.is_main))
    } else {
      None
    };

    let loader = options
      .module_loader
      .unwrap_or_else(|| Rc::new(NoopModuleLoader));

    let module_map = Rc::new(ModuleMap::new(loader));
    if let Some(snapshotted_data) = snapshotted_data {
      module_map.update_with_snapshotted_data(scope, snapshotted_data);
    }
    context.set_slot(scope, module_map.clone());

    let main_realm = {
      let main_realm = JsRealmInner::new(
        context_state,
        main_context,
        module_map.clone(),
        state_rc.clone(),
      );
      let mut state = state_rc.borrow_mut();
      state.inspector = inspector;
      main_realm
    };
    let main_realm = JsRealm::new(main_realm);
    scope.set_data(
      STATE_DATA_OFFSET,
      Rc::into_raw(state_rc.clone()) as *mut c_void,
    );

    drop(context_scope);

    let mut js_runtime = JsRuntime {
      inner: InnerIsolateState {
        will_snapshot,
        main_realm: Some(main_realm),
        state: ManuallyDropRc(ManuallyDrop::new(state_rc)),
        v8_isolate: ManuallyDrop::new(isolate),
      },
      init_mode,
      allocations: IsolateAllocations::default(),
      event_loop_middlewares,
      extensions: options.extensions,
      is_main_runtime: options.is_main,
    };

    let realm = js_runtime.main_realm();
    // TODO(mmastrac): We should thread errors back out of the runtime
    js_runtime.init_extension_js(&realm).unwrap();

    // If the embedder has requested to clear the module map resulting from
    // extensions, possibly with exceptions.
    if let Some(preserve_snapshotted_modules) =
      options.preserve_snapshotted_modules
    {
      module_map.clear_module_map(preserve_snapshotted_modules);
    }

    js_runtime
  }

  #[cfg(test)]
  #[inline]
  pub(crate) fn module_map(&mut self) -> Rc<ModuleMap> {
    self.main_realm().0.module_map()
  }

  #[inline]
  pub fn main_context(&self) -> v8::Global<v8::Context> {
    self.inner.main_realm.as_ref().unwrap().0.context().clone()
  }

  #[inline]
  pub fn v8_isolate(&mut self) -> &mut v8::OwnedIsolate {
    &mut self.inner.v8_isolate
  }

  #[inline]
  pub fn inspector(&mut self) -> Rc<RefCell<JsRuntimeInspector>> {
    self.inner.state.borrow().inspector()
  }

  #[inline]
  pub fn main_realm(&mut self) -> JsRealm {
    self.inner.main_realm.as_ref().unwrap().clone()
  }

  /// Returns the extensions that this runtime is using (including internal ones).
  pub fn extensions(&self) -> &Vec<Extension> {
    &self.extensions
  }

  #[inline]
  pub fn handle_scope(&mut self) -> v8::HandleScope {
    self.main_realm().handle_scope(self.v8_isolate())
  }

  /// Initializes JS of provided Extensions in the given realm.
  fn init_extension_js(&mut self, realm: &JsRealm) -> Result<(), Error> {
    // Initialization of JS happens in phases:
    // 1. Iterate through all extensions:
    //  a. Execute all extension "script" JS files
    //  b. Load all extension "module" JS files (but do not execute them yet)
    // 2. Iterate through all extensions:
    //  a. If an extension has a `esm_entry_point`, execute it.

    let module_map = realm.0.module_map();

    // Take extensions temporarily so we can avoid have a mutable reference to self
    let extensions = std::mem::take(&mut self.extensions);

    let loader = module_map.loader.borrow().clone();
    let ext_loader = Rc::new(ExtModuleLoader::new(&extensions));
    *module_map.loader.borrow_mut() = ext_loader;

    let mut esm_entrypoints = vec![];

    futures::executor::block_on(async {
      if self.init_mode == InitMode::New {
        for file_source in &BUILTIN_SOURCES {
          realm.execute_script(
            self.v8_isolate(),
            file_source.specifier,
            file_source.load()?,
          )?;
        }
      }
      self.init_cbs(realm);

      for extension in &extensions {
        let maybe_esm_entry_point = extension.get_esm_entry_point();

        for file_source in extension.get_esm_sources() {
          realm
            .load_side_module(
              self.v8_isolate(),
              &ModuleSpecifier::parse(file_source.specifier)?,
              None,
            )
            .await?;
        }

        if let Some(entry_point) = maybe_esm_entry_point {
          esm_entrypoints.push(entry_point);
        }

        for file_source in extension.get_js_sources() {
          realm.execute_script(
            self.v8_isolate(),
            file_source.specifier,
            file_source.load()?,
          )?;
        }
      }

      for specifier in esm_entrypoints {
        let mod_id = {
          module_map
            .get_id(specifier, AssertedModuleType::JavaScriptOrWasm)
            .unwrap_or_else(|| {
              panic!("{} not present in the module map", specifier)
            })
        };
        let mut receiver = realm.mod_evaluate(self.v8_isolate(), mod_id);
        realm.evaluate_pending_module(self.v8_isolate());

        // After evaluate_pending_module, if the module isn't fully evaluated
        // and the resolver solved, it means the module or one of its imports
        // uses TLA.
        match receiver.try_recv()? {
          Some(result) => {
            result
              .with_context(|| format!("Couldn't execute '{specifier}'"))?;
          }
          None => {
            // Find the TLA location to display it on the panic.
            let location = {
              let scope = &mut realm.handle_scope(self.v8_isolate());
              let messages = module_map.find_stalled_top_level_await(scope);
              assert!(!messages.is_empty());
              let msg = v8::Local::new(scope, &messages[0]);
              let js_error = JsError::from_v8_message(scope, msg);
              js_error
                .frames
                .first()
                .unwrap()
                .maybe_format_location()
                .unwrap()
            };
            panic!("Top-level await is not allowed in extensions ({location})");
          }
        }
      }

      #[cfg(debug_assertions)]
      {
        let mut scope = realm.handle_scope(self.v8_isolate());
        module_map.assert_all_modules_evaluated(&mut scope);
      }

      Ok::<_, anyhow::Error>(())
    })?;

    self.extensions = extensions;
    let module_map = realm.0.module_map();
    *module_map.loader.borrow_mut() = loader;

    Ok(())
  }

  /// Collects ops from extensions & applies middleware
  fn collect_ops(exts: &mut [Extension]) -> Vec<OpDecl> {
    for (ext, previous_exts) in
      exts.iter().enumerate().map(|(i, ext)| (ext, &exts[..i]))
    {
      ext.check_dependencies(previous_exts);
    }

    // Middleware
    let middleware: Vec<Box<OpMiddlewareFn>> = exts
      .iter_mut()
      .filter_map(|e| e.take_middleware())
      .collect();

    // macroware wraps an opfn in all the middleware
    let macroware = move |d| middleware.iter().fold(d, |d, m| m(d));

    // Flatten ops, apply middleware & override disabled ops
    let ops: Vec<_> = exts
      .iter_mut()
      .flat_map(|e| e.init_ops())
      .map(|d| OpDecl {
        name: d.name,
        ..macroware(*d)
      })
      .collect();

    // In debug build verify there are no duplicate ops.
    #[cfg(debug_assertions)]
    {
      let mut count_by_name = HashMap::new();

      for op in ops.iter() {
        count_by_name
          .entry(&op.name)
          .or_insert(vec![])
          .push(op.name.to_string());
      }

      let mut duplicate_ops = vec![];
      for (op_name, _count) in
        count_by_name.iter().filter(|(_k, v)| v.len() > 1)
      {
        duplicate_ops.push(op_name.to_string());
      }
      if !duplicate_ops.is_empty() {
        let mut msg = "Found ops with duplicate names:\n".to_string();
        for op_name in duplicate_ops {
          msg.push_str(&format!("  - {}\n", op_name));
        }
        msg.push_str("Op names need to be unique.");
        panic!("{}", msg);
      }
    }

    ops
  }

  /// Initializes ops of provided Extensions
  fn create_opstate(options: &mut RuntimeOptions) -> (OpState, Vec<OpDecl>) {
    // Add built-in extension
    options
      .extensions
      .insert(0, crate::ops_builtin::core::init_ops());

    let ops = Self::collect_ops(&mut options.extensions);

    let mut op_state = OpState::new(options.feature_checker.take());

    // Setup state
    for e in &mut options.extensions {
      // ops are already registered during in bindings::initialize_context();
      e.take_state(&mut op_state);
    }

    (op_state, ops)
  }

  pub fn eval<'s, T>(
    scope: &mut v8::HandleScope<'s>,
    code: &str,
  ) -> Option<v8::Local<'s, T>>
  where
    v8::Local<'s, T>: TryFrom<v8::Local<'s, v8::Value>, Error = v8::DataError>,
  {
    let scope = &mut v8::EscapableHandleScope::new(scope);
    let source = v8::String::new(scope, code).unwrap();
    let script = v8::Script::compile(scope, source, None).unwrap();
    let v = script.run(scope)?;
    scope.escape(v).try_into().ok()
  }

  /// Grabs a reference to core.js' eventLoopTick & buildCustomError
  fn init_cbs(&mut self, realm: &JsRealm) {
    let (event_loop_tick_cb, build_custom_error_cb) = {
      let scope = &mut realm.handle_scope(self.v8_isolate());
      let context = realm.context();
      let context_local = v8::Local::new(scope, context);
      let global = context_local.global(scope);
      let deno_str =
        v8::String::new_external_onebyte_static(scope, b"Deno").unwrap();
      let core_str =
        v8::String::new_external_onebyte_static(scope, b"core").unwrap();
      let event_loop_tick_str =
        v8::String::new_external_onebyte_static(scope, b"eventLoopTick")
          .unwrap();
      let build_custom_error_str =
        v8::String::new_external_onebyte_static(scope, b"buildCustomError")
          .unwrap();

      let deno_obj: v8::Local<v8::Object> = global
        .get(scope, deno_str.into())
        .unwrap()
        .try_into()
        .unwrap();
      let core_obj: v8::Local<v8::Object> = deno_obj
        .get(scope, core_str.into())
        .unwrap()
        .try_into()
        .unwrap();

      let event_loop_tick_cb: v8::Local<v8::Function> = core_obj
        .get(scope, event_loop_tick_str.into())
        .unwrap()
        .try_into()
        .unwrap();
      let build_custom_error_cb: v8::Local<v8::Function> = core_obj
        .get(scope, build_custom_error_str.into())
        .unwrap()
        .try_into()
        .unwrap();
      (
        v8::Global::new(scope, event_loop_tick_cb),
        v8::Global::new(scope, build_custom_error_cb),
      )
    };

    // Put global handles in the realm's ContextState
    let state_rc = realm.0.state();
    let mut state = state_rc.borrow_mut();
    state
      .js_event_loop_tick_cb
      .replace(Rc::new(event_loop_tick_cb));
    state
      .js_build_custom_error_cb
      .replace(Rc::new(build_custom_error_cb));
  }

  /// Returns the runtime's op state, which can be used to maintain ops
  /// and access resources between op calls.
  pub fn op_state(&mut self) -> Rc<RefCell<OpState>> {
    let state = self.inner.state.borrow();
    state.op_state.clone()
  }

  /// Returns the runtime's op names, ordered by OpId.
  pub fn op_names(&self) -> Vec<&'static str> {
    let main_realm = self.inner.main_realm.as_ref().unwrap().clone();
    let state_rc = main_realm.0.state();
    let state = state_rc.borrow();
    state.op_ctxs.iter().map(|o| o.decl.name).collect()
  }

  /// Executes traditional JavaScript code (traditional = not ES modules).
  ///
  /// The execution takes place on the current main realm, so it is possible
  /// to maintain local JS state and invoke this method multiple times.
  ///
  /// `name` can be a filepath or any other string, but it is required to be 7-bit ASCII, eg.
  ///
  ///   - "/some/file/path.js"
  ///   - "<anon>"
  ///   - "[native code]"
  ///
  /// The same `name` value can be used for multiple executions.
  ///
  /// `Error` can usually be downcast to `JsError`.
  pub fn execute_script(
    &mut self,
    name: &'static str,
    source_code: ModuleCode,
  ) -> Result<v8::Global<v8::Value>, Error> {
    self
      .main_realm()
      .execute_script(self.v8_isolate(), name, source_code)
  }

  /// Executes traditional JavaScript code (traditional = not ES modules).
  ///
  /// The execution takes place on the current main realm, so it is possible
  /// to maintain local JS state and invoke this method multiple times.
  ///
  /// `name` can be a filepath or any other string, but it is required to be 7-bit ASCII, eg.
  ///
  ///   - "/some/file/path.js"
  ///   - "<anon>"
  ///   - "[native code]"
  ///
  /// The same `name` value can be used for multiple executions.
  ///
  /// `Error` can usually be downcast to `JsError`.
  pub fn execute_script_static(
    &mut self,
    name: &'static str,
    source_code: &'static str,
  ) -> Result<v8::Global<v8::Value>, Error> {
    self.main_realm().execute_script(
      self.v8_isolate(),
      name,
      ModuleCode::from_static(source_code),
    )
  }

  /// Call a function. If it returns a promise, run the event loop until that
  /// promise is settled. If the promise rejects or there is an uncaught error
  /// in the event loop, return `Err(error)`. Or return `Ok(<await returned>)`.
  pub async fn call_and_await(
    &mut self,
    function: &v8::Global<v8::Function>,
  ) -> Result<v8::Global<v8::Value>, Error> {
    let promise = {
      let scope = &mut self.handle_scope();
      let cb = function.open(scope);
      let this = v8::undefined(scope).into();
      let promise = cb.call(scope, this, &[]);
      if promise.is_none() || scope.is_execution_terminating() {
        let undefined = v8::undefined(scope).into();
        return exception_to_err_result(scope, undefined, false);
      }
      v8::Global::new(scope, promise.unwrap())
    };
    let result = self.resolve_value(promise).await;
    let scope = &mut self.handle_scope();
    // TODO(mmastrac)
    // self.check_promise_rejections(scope)?;
    result
  }

  /// Returns the namespace object of a module.
  ///
  /// This is only available after module evaluation has completed.
  /// This function panics if module has not been instantiated.
  pub fn get_module_namespace(
    &mut self,
    module_id: ModuleId,
  ) -> Result<v8::Global<v8::Object>, Error> {
    self
      .main_realm()
      .get_module_namespace(self.v8_isolate(), module_id)
  }

  /// Registers a callback on the isolate when the memory limits are approached.
  /// Use this to prevent V8 from crashing the process when reaching the limit.
  ///
  /// Calls the closure with the current heap limit and the initial heap limit.
  /// The return value of the closure is set as the new limit.
  pub fn add_near_heap_limit_callback<C>(&mut self, cb: C)
  where
    C: FnMut(usize, usize) -> usize + 'static,
  {
    let boxed_cb = Box::new(RefCell::new(cb));
    let data = boxed_cb.as_ptr() as *mut c_void;

    let prev = self
      .allocations
      .near_heap_limit_callback_data
      .replace((boxed_cb, near_heap_limit_callback::<C>));
    if let Some((_, prev_cb)) = prev {
      self
        .v8_isolate()
        .remove_near_heap_limit_callback(prev_cb, 0);
    }

    self
      .v8_isolate()
      .add_near_heap_limit_callback(near_heap_limit_callback::<C>, data);
  }

  pub fn remove_near_heap_limit_callback(&mut self, heap_limit: usize) {
    if let Some((_, cb)) = self.allocations.near_heap_limit_callback_data.take()
    {
      self
        .v8_isolate()
        .remove_near_heap_limit_callback(cb, heap_limit);
    }
  }

  fn pump_v8_message_loop(
    &mut self,
    scope: &mut v8::HandleScope,
  ) -> Result<(), Error> {
    while v8::Platform::pump_message_loop(
      &v8::V8::get_current_platform(),
      scope,
      false, // don't block if there are no tasks
    ) {
      // do nothing
    }

    let tc_scope = &mut v8::TryCatch::new(scope);
    tc_scope.perform_microtask_checkpoint();
    match tc_scope.exception() {
      None => Ok(()),
      Some(exception) => exception_to_err_result(tc_scope, exception, false),
    }
  }

  pub fn maybe_init_inspector(&mut self) {
    if self.inner.state.borrow().inspector.is_some() {
      return;
    }

    let context = self.main_context();
    let scope = &mut v8::HandleScope::with_context(
      self.inner.v8_isolate.as_mut(),
      context.clone(),
    );
    let context = v8::Local::new(scope, context);

    let mut state = self.inner.state.borrow_mut();
    state.inspector = Some(JsRuntimeInspector::new(
      scope,
      context,
      self.is_main_runtime,
    ));
  }

  pub fn poll_value(
    &mut self,
    global: &v8::Global<v8::Value>,
    cx: &mut Context,
  ) -> Poll<Result<v8::Global<v8::Value>, Error>> {
    // Check if the value is not a promise or already settled before polling the
    // event loop.
    let promise_global = {
      let scope = &mut self.handle_scope();
      let local = v8::Local::<v8::Value>::new(scope, global);
      if let Ok(promise) = v8::Local::<v8::Promise>::try_from(local) {
        match promise.state() {
          v8::PromiseState::Pending => v8::Global::new(scope, promise),
          v8::PromiseState::Fulfilled => {
            let value = promise.result(scope);
            let value_handle = v8::Global::new(scope, value);
            return Poll::Ready(Ok(value_handle));
          }
          v8::PromiseState::Rejected => {
            let exception = promise.result(scope);
            let promise_global = v8::Global::new(scope, promise);
            JsRealm::state_from_scope(scope)
              .borrow_mut()
              .pending_promise_rejections
              .retain(|(key, _)| key != &promise_global);
            return Poll::Ready(exception_to_err_result(
              scope, exception, false,
            ));
          }
        }
      } else {
        return Poll::Ready(Ok(global.clone()));
      }
    };

    // Poll the event loop.
    let event_loop_result = self.poll_event_loop(cx, false)?;

    // Check the promise state again.
    let scope = &mut self.handle_scope();
    let promise = v8::Local::new(scope, promise_global);
    match promise.state() {
      v8::PromiseState::Pending => match event_loop_result {
        Poll::Ready(_) => {
          let msg = "Promise resolution is still pending but the event loop has already resolved.";
          Poll::Ready(Err(generic_error(msg)))
        }
        Poll::Pending => Poll::Pending,
      },
      v8::PromiseState::Fulfilled => {
        let value = promise.result(scope);
        let value_handle = v8::Global::new(scope, value);
        Poll::Ready(Ok(value_handle))
      }
      v8::PromiseState::Rejected => {
        let exception = promise.result(scope);
        Poll::Ready(exception_to_err_result(scope, exception, false))
      }
    }
  }

  /// Waits for the given value to resolve while polling the event loop.
  ///
  /// This future resolves when either the value is resolved or the event loop runs to
  /// completion.
  pub async fn resolve_value(
    &mut self,
    global: v8::Global<v8::Value>,
  ) -> Result<v8::Global<v8::Value>, Error> {
    poll_fn(|cx| self.poll_value(&global, cx)).await
  }

  /// Runs event loop to completion
  ///
  /// This future resolves when:
  ///  - there are no more pending dynamic imports
  ///  - there are no more pending ops
  ///  - there are no more active inspector sessions (only if `wait_for_inspector` is set to true)
  pub async fn run_event_loop(
    &mut self,
    wait_for_inspector: bool,
  ) -> Result<(), Error> {
    poll_fn(|cx| self.poll_event_loop(cx, wait_for_inspector)).await
  }

  /// Runs event loop to completion
  ///
  /// This future resolves when:
  ///  - there are no more pending dynamic imports
  ///  - there are no more pending ops
  ///  - there are no more active inspector sessions (only if
  ///     `PollEventLoopOptions.wait_for_inspector` is set to true)
  pub async fn run_event_loop2(
    &mut self,
    poll_options: PollEventLoopOptions,
  ) -> Result<(), Error> {
    poll_fn(|cx| self.poll_event_loop2(cx, poll_options)).await
  }

  /// A utility function that run provided future concurrently with the event loop.
  ///
  /// Useful for interacting with local inspector session.
  pub async fn with_event_loop<'fut, T>(
    &mut self,
    mut fut: Pin<Box<dyn Future<Output = T> + 'fut>>,
    poll_options: PollEventLoopOptions,
  ) -> T {
    loop {
      tokio::select! {
        biased;

        result = &mut fut => {
          return result;
        },

        _ = self.run_event_loop2(poll_options) => {}
      }
    }
  }

  /// Runs a single tick of event loop
  ///
  /// If `wait_for_inspector` is set to true event loop
  /// will return `Poll::Pending` if there are active inspector sessions.
  pub fn poll_event_loop(
    &mut self,
    cx: &mut Context,
    wait_for_inspector: bool,
  ) -> Poll<Result<(), Error>> {
    self.poll_event_loop_inner(
      cx,
      PollEventLoopOptions {
        wait_for_inspector,
        ..Default::default()
      },
    )
  }

  /// Runs a single tick of event loop
  ///
  /// If `PollEventLoopOptions.wait_for_inspector` is set to true, the event
  /// loop will return `Poll::Pending` if there are active inspector sessions.
  pub fn poll_event_loop2(
    &mut self,
    cx: &mut Context,
    poll_options: PollEventLoopOptions,
  ) -> Poll<Result<(), Error>> {
    self.poll_event_loop_inner(cx, poll_options)
  }

  fn poll_event_loop_inner(
    &mut self,
    cx: &mut Context,
    poll_options: PollEventLoopOptions,
  ) -> Poll<Result<(), Error>> {
    // TODO(mmastrac): We want a scope that will still allow us to call methods on self. The need to do this
    // probably indicates that this code needs to be refactored further.
    let isolate = &mut **self.inner.v8_isolate as *mut v8::Isolate;
    let mut isolate_scope =
      v8::HandleScope::new(unsafe { isolate.as_mut().unwrap_unchecked() });
    let context =
      v8::Local::new(&mut isolate_scope, self.main_realm().context());
    let mut scope = v8::ContextScope::new(&mut isolate_scope, context);

    let has_inspector: bool;
    {
      let state = self.inner.state.borrow();
      has_inspector = state.inspector.is_some();
      state.op_state.borrow().waker.register(cx.waker());
    }

    if has_inspector {
      // We poll the inspector first.
      let _ = self.inspector().borrow().poll_sessions(Some(cx)).unwrap();
    }

    if poll_options.pump_v8_message_loop {
      self.pump_v8_message_loop(&mut scope)?;
    }

    // Dynamic module loading - ie. modules loaded using "import()"
    {
      // Run in a loop so that dynamic imports that only depend on another
      // dynamic import can be resolved in this event loop iteration.
      //
      // For example, a dynamically imported module like the following can be
      // immediately resolved after `dependency.ts` is fully evaluated, but it
      // wouldn't if not for this loop.
      //
      //    await delay(1000);
      //    await import("./dependency.ts");
      //    console.log("test")
      //
      // These dynamic import dependencies can be cross-realm:
      //
      //    await delay(1000);
      //    await new ShadowRealm().importValue("./dependency.js", "default");
      //
      loop {
        let mut has_evaluated = false;

        // Fast main realm poll
        {
          let main_realm = self.inner.main_realm.as_ref().unwrap();
          let modules = main_realm.0.module_map();
          loop {
            let poll_imports =
              modules.poll_prepare_dyn_imports(&mut scope, cx)?;
            assert!(poll_imports.is_ready());

            let poll_imports = modules.poll_dyn_imports(&mut scope, cx)?;
            assert!(poll_imports.is_ready());

            if modules.evaluate_dyn_imports(&mut scope) {
              has_evaluated = true;
            } else {
              break;
            }
          }
        }

        if !has_evaluated {
          break;
        }
      }
    }

    // Resolve async ops, run all next tick callbacks and macrotasks callbacks
    // and only then check for any promise exceptions (`unhandledrejection`
    // handlers are run in macrotasks callbacks so we need to let them run
    // first).
    let dispatched_ops = self.do_js_event_loop_tick(cx, &mut scope)?;
    self.check_promise_rejections(&mut scope)?;

    // Event loop middlewares
    let mut maybe_scheduling = false;
    {
      let op_state = self.inner.state.borrow().op_state.clone();
      for f in &self.event_loop_middlewares {
        if f(op_state.clone(), cx) {
          maybe_scheduling = true;
        }
      }
    }

    // Top level module
    {
      self
        .inner
        .main_realm
        .as_ref()
        .unwrap()
        .evaluate_pending_module(&mut scope);
    }

    // Get the pending state from the main realm, or all realms
    let pending_state = {
      let inner = &self.inner.main_realm.as_ref().unwrap().0;
      EventLoopPendingState::new(
        &mut scope,
        &self.inner.state.borrow(),
        &inner.state().borrow(),
        &inner.module_map(),
      )
    };

    if !pending_state.is_pending() && !maybe_scheduling {
      if has_inspector {
        let inspector = self.inspector();
        let has_active_sessions = inspector.borrow().has_active_sessions();
        let has_blocking_sessions = inspector.borrow().has_blocking_sessions();

        if poll_options.wait_for_inspector && has_active_sessions {
          // If there are no blocking sessions (eg. REPL) we can now notify
          // debugger that the program has finished running and we're ready
          // to exit the process once debugger disconnects.
          if !has_blocking_sessions {
            let context = self.main_context();
            let scope = &mut self.handle_scope();
            inspector.borrow_mut().context_destroyed(scope, context);
            println!("Program finished. Waiting for inspector to disconnect to exit the process...");
          }

          return Poll::Pending;
        }
      }

      return Poll::Ready(Ok(()));
    }

    // Check if more async ops have been dispatched
    // during this turn of event loop.
    // If there are any pending background tasks, we also wake the runtime to
    // make sure we don't miss them.
    // TODO(andreubotella) The event loop will spin as long as there are pending
    // background tasks. We should look into having V8 notify us when a
    // background task is done.
    #[allow(clippy::suspicious_else_formatting, clippy::if_same_then_else)]
    {
      if pending_state.has_pending_background_tasks
        || pending_state.has_tick_scheduled
        || maybe_scheduling
      {
        let state = self.inner.state.borrow();
        state.op_state.borrow().waker.wake();
      } else
      // If ops were dispatched we may have progress on pending modules that we should re-check
      if (pending_state.has_pending_module_evaluation
        || pending_state.has_pending_dyn_module_evaluation)
        && dispatched_ops
      {
        let state = self.inner.state.borrow();
        state.op_state.borrow().waker.wake();
      }
    }

    if pending_state.has_pending_module_evaluation {
      if pending_state.has_pending_refed_ops
        || pending_state.has_pending_dyn_imports
        || pending_state.has_pending_dyn_module_evaluation
        || pending_state.has_pending_background_tasks
        || pending_state.has_tick_scheduled
        || maybe_scheduling
      {
        // pass, will be polled again
      } else {
        let known_realms = &[self.inner.main_realm.as_ref().unwrap().0.clone()];
        return Poll::Ready(Err(
          find_and_report_stalled_level_await_in_any_realm(
            &mut scope,
            known_realms,
          ),
        ));
      }
    }

    if pending_state.has_pending_dyn_module_evaluation {
      if pending_state.has_pending_refed_ops
        || pending_state.has_pending_dyn_imports
        || pending_state.has_pending_background_tasks
        || pending_state.has_tick_scheduled
      {
        // pass, will be polled again
      } else if self.inner.main_realm.as_ref().unwrap().modules_idle() {
        let known_realms = &[self.inner.main_realm.as_ref().unwrap().0.clone()];
        return Poll::Ready(Err(
          find_and_report_stalled_level_await_in_any_realm(
            &mut scope,
            known_realms,
          ),
        ));
      } else {
        let state = self.inner.state.borrow_mut();
        // Delay the above error by one spin of the event loop. A dynamic import
        // evaluation may complete during this, in which case the counter will
        // reset.
        self
          .inner
          .main_realm
          .as_ref()
          .unwrap()
          .increment_modules_idle();
        state.op_state.borrow().waker.wake();
      }
    }

    Poll::Pending
  }
}

fn find_and_report_stalled_level_await_in_any_realm(
  v8_isolate: &mut v8::Isolate,
  known_realms: &[JsRealmInner],
) -> Error {
  for inner_realm in known_realms {
    let scope = &mut inner_realm.handle_scope(v8_isolate);
    let module_map = inner_realm.module_map();
    let messages = module_map.find_stalled_top_level_await(scope);

    if !messages.is_empty() {
      // We are gonna print only a single message to provide a nice formatting
      // with source line of offending promise shown. Once user fixed it, then
      // they will get another error message for the next promise (but this
      // situation is gonna be very rare, if ever happening).
      let msg = v8::Local::new(scope, &messages[0]);
      let js_error = JsError::from_v8_message(scope, msg);
      return js_error.into();
    }
  }

  unreachable!("Expected at least one stalled top-level await");
}

fn create_context<'a>(
  scope: &mut v8::HandleScope<'a, ()>,
  global_template_middlewares: &[GlobalTemplateMiddlewareFn],
  global_object_middlewares: &[GlobalObjectMiddlewareFn],
) -> v8::Local<'a, v8::Context> {
  // Set up the global object template and create context from it.
  let mut global_object_template = v8::ObjectTemplate::new(scope);
  for middleware in global_template_middlewares {
    global_object_template = middleware(scope, global_object_template);
  }
  let context = v8::Context::new_from_template(scope, global_object_template);
  let scope = &mut v8::ContextScope::new(scope, context);

  // Get the global wrapper object from the context, get the real inner
  // global object from it, and and configure it using the middlewares.
  let global_wrapper = context.global(scope);
  let real_global = global_wrapper
    .get_prototype(scope)
    .unwrap()
    .to_object(scope)
    .unwrap();
  for middleware in global_object_middlewares {
    middleware(scope, real_global);
  }
  context
}

impl JsRuntimeForSnapshot {
  pub fn new(mut options: RuntimeOptions) -> JsRuntimeForSnapshot {
    JsRuntime::init_v8(
      options.v8_platform.take(),
      true,
      options.unsafe_expose_natives_and_gc(),
    );
    JsRuntimeForSnapshot(JsRuntime::new_inner(options, true))
  }

  /// Takes a snapshot and consumes the runtime.
  ///
  /// `Error` can usually be downcast to `JsError`.
  pub fn snapshot(mut self) -> v8::StartupData {
    // Ensure there are no live inspectors to prevent crashes.
    self.inner.prepare_for_cleanup();

    let realm = self.main_realm();

    // Set the context to be snapshot's default context
    {
      let mut scope = realm.handle_scope(self.v8_isolate());
      let local_context = v8::Local::new(&mut scope, realm.context());
      scope.set_default_context(local_context);
    }

    // Serialize the module map and store its data in the snapshot.
    {
      let snapshotted_data = {
        let module_map = realm.0.module_map();
        module_map.serialize_for_snapshotting(
          &mut realm.handle_scope(self.v8_isolate()),
        )
      };

      let mut scope = realm.handle_scope(self.v8_isolate());
      snapshot_util::set_snapshotted_data(
        &mut scope,
        realm.context().clone(),
        snapshotted_data,
      );
    }

    drop(realm);

    self
      .0
      .inner
      .prepare_for_snapshot()
      .create_blob(v8::FunctionCodeHandling::Keep)
      .unwrap()
  }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct EventLoopPendingState {
  pub(crate) has_pending_refed_ops: bool,
  pub(crate) has_pending_dyn_imports: bool,
  pub(crate) has_pending_dyn_module_evaluation: bool,
  pub(crate) has_pending_module_evaluation: bool,
  pub(crate) has_pending_background_tasks: bool,
  pub(crate) has_tick_scheduled: bool,
}

impl EventLoopPendingState {
  /// Collect event loop state from all the sub-states.
  pub fn new(
    scope: &mut v8::HandleScope<()>,
    runtime_state: &JsRuntimeState,
    state: &ContextState,
    modules: &ModuleMap,
  ) -> Self {
    let num_unrefed_ops = state.unrefed_ops.len();
    let num_pending_ops = state.pending_ops.len();
    let has_pending_dyn_imports = modules.has_pending_dynamic_imports();
    let has_pending_dyn_module_evaluation =
      modules.has_pending_dyn_module_evaluation();
    let has_pending_module_evaluation = state.pending_mod_evaluate.is_some();
    EventLoopPendingState {
      has_pending_refed_ops: num_pending_ops > num_unrefed_ops,
      has_pending_dyn_imports,
      has_pending_dyn_module_evaluation,
      has_pending_module_evaluation,
      has_pending_background_tasks: scope.has_pending_background_tasks(),
      has_tick_scheduled: runtime_state.has_tick_scheduled,
    }
  }

  /// Collect event loop state from all the states stored in the scope.
  pub fn new_from_scope(scope: &mut v8::HandleScope) -> Self {
    let module_map = JsRealm::module_map_from(scope);
    let context_state = JsRealm::state_from_scope(scope);
    let context_state = context_state.borrow();
    let runtime_state = JsRuntime::state_from(scope);
    let runtime_state = runtime_state.borrow();
    Self::new(scope, &runtime_state, &context_state, &module_map)
  }

  pub fn is_pending(&self) -> bool {
    self.has_pending_refed_ops
      || self.has_pending_dyn_imports
      || self.has_pending_dyn_module_evaluation
      || self.has_pending_module_evaluation
      || self.has_pending_background_tasks
      || self.has_tick_scheduled
  }
}

extern "C" fn near_heap_limit_callback<F>(
  data: *mut c_void,
  current_heap_limit: usize,
  initial_heap_limit: usize,
) -> usize
where
  F: FnMut(usize, usize) -> usize,
{
  // SAFETY: The data is a pointer to the Rust callback function. It is stored
  // in `JsRuntime::allocations` and thus is guaranteed to outlive the isolate.
  let callback = unsafe { &mut *(data as *mut F) };
  callback(current_heap_limit, initial_heap_limit)
}

impl JsRuntimeState {
  pub(crate) fn inspector(&self) -> Rc<RefCell<JsRuntimeInspector>> {
    self.inspector.as_ref().unwrap().clone()
  }

  /// Called by `bindings::host_import_module_dynamically_callback`
  /// after initiating new dynamic import load.
  pub fn notify_new_dynamic_import(&mut self) {
    // Notify event loop to poll again soon.
    self.op_state.borrow().waker.wake();
  }
}

// Related to module loading
impl JsRuntime {
  #[cfg(test)]
  pub(crate) fn instantiate_module(
    &mut self,
    id: ModuleId,
  ) -> Result<(), v8::Global<v8::Value>> {
    self.main_realm().instantiate_module(self.v8_isolate(), id)
  }

  // TODO(bartlomieju): make it return `ModuleEvaluationFuture`?
  /// Evaluates an already instantiated ES module.
  ///
  /// Returns a receiver handle that resolves when module promise resolves.
  /// Implementors must manually call [`JsRuntime::run_event_loop`] to drive
  /// module evaluation future.
  ///
  /// `Error` can usually be downcast to `JsError` and should be awaited and
  /// checked after [`JsRuntime::run_event_loop`] completion.
  ///
  /// This function panics if module has not been instantiated.
  pub fn mod_evaluate(
    &mut self,
    id: ModuleId,
  ) -> oneshot::Receiver<Result<(), Error>> {
    self.main_realm().mod_evaluate(self.v8_isolate(), id)
  }

  /// Asynchronously load specified module and all of its dependencies.
  ///
  /// The module will be marked as "main", and because of that
  /// "import.meta.main" will return true when checked inside that module.
  ///
  /// User must call [`JsRuntime::mod_evaluate`] with returned `ModuleId`
  /// manually after load is finished.
  pub async fn load_main_module(
    &mut self,
    specifier: &ModuleSpecifier,
    code: Option<ModuleCode>,
  ) -> Result<ModuleId, Error> {
    self
      .main_realm()
      .load_main_module(self.v8_isolate(), specifier, code)
      .await
  }

  /// Asynchronously load specified ES module and all of its dependencies.
  ///
  /// This method is meant to be used when loading some utility code that
  /// might be later imported by the main module (ie. an entry point module).
  ///
  /// User must call [`JsRuntime::mod_evaluate`] with returned `ModuleId`
  /// manually after load is finished.
  pub async fn load_side_module(
    &mut self,
    specifier: &ModuleSpecifier,
    code: Option<ModuleCode>,
  ) -> Result<ModuleId, Error> {
    self
      .main_realm()
      .load_side_module(self.v8_isolate(), specifier, code)
      .await
  }

  fn check_promise_rejections(
    &self,
    scope: &mut v8::HandleScope,
  ) -> Result<(), Error> {
    self
      .inner
      .main_realm
      .as_ref()
      .unwrap()
      .0
      .check_promise_rejections(scope)
  }

  // Polls pending ops and then runs `Deno.core.eventLoopTick` callback.
  fn do_js_event_loop_tick(
    &mut self,
    cx: &mut Context,
    scope: &mut v8::HandleScope,
  ) -> Result<bool, Error> {
    // Handle responses for each realm.
    let state = self.inner.state.clone();

    let dispatched_ops = Self::do_js_event_loop_tick_realm(
      cx,
      scope,
      &state,
      &self.inner.main_realm.as_ref().unwrap().0,
    )?;

    Ok(dispatched_ops)
  }

  fn do_js_event_loop_tick_realm(
    cx: &mut Context,
    scope: &mut v8::HandleScope,
    state: &Rc<RefCell<JsRuntimeState>>,
    realm: &JsRealmInner,
  ) -> Result<bool, Error> {
    let context_state = realm.state();
    let mut context_state = context_state.borrow_mut();
    let mut dispatched_ops = false;

    // We return async responses to JS in unbounded batches (may change),
    // each batch is a flat vector of tuples:
    // `[promise_id1, op_result1, promise_id2, op_result2, ...]`
    // promise_id is a simple integer, op_result is an ops::OpResult
    // which contains a value OR an error, encoded as a tuple.
    // This batch is received in JS via the special `arguments` variable
    // and then each tuple is used to resolve or reject promises
    //
    // This can handle 15 promises futures in a single batch without heap
    // allocations.
    let mut args: SmallVec<[v8::Local<v8::Value>; 32]> =
      SmallVec::with_capacity(32);

    loop {
      let Poll::Ready(item) = context_state.pending_ops.poll_join_next(cx) else {
        break;
      };
      // TODO(mmastrac): If this task is really errored, things could be pretty bad
      let PendingOp(promise_id, op_id, resp, metrics_event) = item.unwrap();
      context_state.unrefed_ops.remove(&promise_id);
      dispatched_ops |= true;
      args.push(v8::Integer::new(scope, promise_id).into());
      let was_error = matches!(resp, OpResult::Err(_));
      let res = resp.to_v8(scope);
      if metrics_event {
        if res.is_ok() && !was_error {
          dispatch_metrics_async(
            &context_state.op_ctxs[op_id as usize],
            OpMetricsEvent::CompletedAsync,
          );
        } else {
          dispatch_metrics_async(
            &context_state.op_ctxs[op_id as usize],
            OpMetricsEvent::ErrorAsync,
          );
        }
      }
      args.push(match res {
        Ok(v) => v,
        Err(e) => OpResult::Err(OpError::new(&|_| "TypeError", e.into()))
          .to_v8(scope)
          .unwrap(),
      });
    }

    let has_tick_scheduled = state.borrow().has_tick_scheduled;
    dispatched_ops |= has_tick_scheduled;
    let has_tick_scheduled = v8::Boolean::new(scope, has_tick_scheduled);
    args.push(has_tick_scheduled.into());

    let js_event_loop_tick_cb_handle =
      context_state.js_event_loop_tick_cb.clone().unwrap();
    let tc_scope = &mut v8::TryCatch::new(scope);
    let js_event_loop_tick_cb = js_event_loop_tick_cb_handle.open(tc_scope);
    let this = v8::undefined(tc_scope).into();
    drop(context_state);
    js_event_loop_tick_cb.call(tc_scope, this, args.as_slice());

    if let Some(exception) = tc_scope.exception() {
      // TODO(@andreubotella): Returning here can cause async ops in other
      // realms to never resolve.
      return exception_to_err_result(tc_scope, exception, false);
    }

    if tc_scope.has_terminated() || tc_scope.is_execution_terminating() {
      return Ok(false);
    }

    Ok(dispatched_ops)
  }
}
