// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use super::bindings;
use super::bindings::create_exports_for_ops_virtual_module;
use super::bindings::v8_static_strings;
use super::bindings::watch_promise;
use super::exception_state::ExceptionState;
use super::jsrealm::JsRealmInner;
use super::op_driver::OpDriver;
use super::setup;
use super::snapshot_util;
use super::stats::RuntimeActivityStatsFactory;
use super::SnapshottedData;
use crate::error::exception_to_err_result;
use crate::error::AnyError;
use crate::error::GetErrorClassFn;
use crate::error::JsError;
use crate::extension_set;
use crate::extensions::GlobalObjectMiddlewareFn;
use crate::extensions::GlobalTemplateMiddlewareFn;
use crate::include_js_files;
use crate::inspector::JsRuntimeInspector;
use crate::module_specifier::ModuleSpecifier;
use crate::modules::default_import_meta_resolve_cb;
use crate::modules::CustomModuleEvaluationCb;
use crate::modules::ExtModuleLoader;
use crate::modules::ImportMetaResolveCallback;
use crate::modules::ModuleCodeString;
use crate::modules::ModuleId;
use crate::modules::ModuleLoader;
use crate::modules::ModuleMap;
use crate::modules::ModuleMapSnapshottedData;
use crate::modules::RequestedModuleType;
use crate::modules::ValidateImportAttributesCb;
use crate::ops_metrics::dispatch_metrics_async;
use crate::ops_metrics::OpMetricsFactoryFn;
use crate::runtime::ContextState;
use crate::runtime::DefaultOpDriver;
use crate::runtime::JsRealm;
use crate::source_map::SourceMapCache;
use crate::source_map::SourceMapGetter;
use crate::Extension;
use crate::ExtensionFileSource;
use crate::FastString;
use crate::FeatureChecker;
use crate::NoopModuleLoader;
use crate::OpMetricsEvent;
use crate::OpState;
use anyhow::anyhow;
use anyhow::bail;
use anyhow::Context as AnyhowContext;
use anyhow::Error;
use futures::future::poll_fn;
use futures::task::noop_waker_ref;
use futures::task::AtomicWaker;
use futures::Future;
use futures::FutureExt;
use smallvec::SmallVec;
use std::any::Any;
use std::cell::Cell;
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::mem::ManuallyDrop;
use std::ops::Deref;
use std::ops::DerefMut;
use std::option::Option;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::Mutex;
use std::task::Context;
use std::task::Poll;
use std::task::Waker;
use v8::Isolate;

pub type WaitForInspectorDisconnectCallback = Box<dyn Fn()>;
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
  #[allow(unused)]
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
  main_realm: ManuallyDrop<JsRealm>,
  pub(crate) state: ManuallyDropRc<JsRuntimeState>,
  v8_isolate: ManuallyDrop<v8::OwnedIsolate>,
  cpp_heap: ManuallyDrop<v8::UniqueRef<v8::cppgc::Heap>>,
}

impl InnerIsolateState {
  /// Clean out the opstate and take the inspector to prevent the inspector from getting destroyed
  /// after we've torn down the contexts. If the inspector is not correctly torn down, random crashes
  /// happen in tests (and possibly for users using the inspector).
  pub fn prepare_for_cleanup(&mut self) {
    let inspector = self.state.inspector.take();
    self.state.op_state.borrow_mut().clear();
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
    _ = unsafe { Rc::from_raw(state_ptr as *const JsRuntimeState) };

    unsafe {
      ManuallyDrop::take(&mut self.main_realm).0.destroy();
    }

    debug_assert_eq!(Rc::strong_count(&self.state), 1);
  }

  pub fn prepare_for_snapshot(mut self) -> v8::OwnedIsolate {
    self.cleanup();
    // SAFETY: We're copying out of self and then immediately forgetting self
    let (state, _cpp_heap, isolate) = unsafe {
      (
        ManuallyDrop::take(&mut self.state.0),
        ManuallyDrop::take(&mut self.cpp_heap),
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
        ManuallyDrop::drop(&mut self.cpp_heap);
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

#[derive(Default)]
struct PromiseFuture {
  resolved: Cell<Option<Result<v8::Global<v8::Value>, Error>>>,
  waker: Cell<Option<Waker>>,
}

#[derive(Clone, Default)]
struct RcPromiseFuture(Rc<PromiseFuture>);

impl RcPromiseFuture {
  pub fn new(res: Result<v8::Global<v8::Value>, Error>) -> Self {
    Self(Rc::new(PromiseFuture {
      resolved: Some(res).into(),
      ..Default::default()
    }))
  }
}

impl Future for RcPromiseFuture {
  type Output = Result<v8::Global<v8::Value>, Error>;
  fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
    let this = self.get_mut();
    if let Some(resolved) = this.0.resolved.take() {
      Poll::Ready(resolved)
    } else {
      this.0.waker.set(Some(cx.waker().clone()));
      Poll::Pending
    }
  }
}

const VIRTUAL_OPS_MODULE_NAME: &str = "ext:core/ops";

/// These files are executed just after a new context is created. They provided
/// the necessary infrastructure to bind ops.
pub(crate) const CONTEXT_SETUP_SOURCES: [ExtensionFileSource; 2] = include_js_files!(
  core
  "00_primordials.js",
  "00_infra.js",
);

/// These files are executed when we start setting up extensions. They rely
/// on ops being already fully set up.
pub(crate) const BUILTIN_SOURCES: [ExtensionFileSource; 2] = include_js_files!(
  core
  "01_core.js",
  "02_error.js",
);
/// Executed after `BUILTIN_SOURCES` are executed. Provides a thin ES module
/// that exports `core`, `internals` and `primordials` objects.
pub(crate) const BUILTIN_ES_MODULES: [ExtensionFileSource; 1] =
  include_js_files!(core "mod.js",);

/// We have `ext:core/ops` and `ext:core/mod.js` that are always provided.
#[cfg(test)]
pub(crate) const NO_OF_BUILTIN_MODULES: usize = 2;

/// A single execution context of JavaScript. Corresponds roughly to the "Web
/// Worker" concept in the DOM.
///
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
  pub(crate) source_map_getter: Option<Rc<Box<dyn SourceMapGetter>>>,
  pub(crate) source_map_cache: Rc<RefCell<SourceMapCache>>,
  pub(crate) op_state: Rc<RefCell<OpState>>,
  pub(crate) shared_array_buffer_store: Option<SharedArrayBufferStore>,
  pub(crate) compiled_wasm_module_store: Option<CompiledWasmModuleStore>,
  wait_for_inspector_disconnect_callback:
    Option<WaitForInspectorDisconnectCallback>,
  pub(crate) validate_import_attributes_cb: Option<ValidateImportAttributesCb>,
  pub(crate) custom_module_evaluation_cb: Option<CustomModuleEvaluationCb>,
  waker: Arc<AtomicWaker>,
  /// Accessed through [`JsRuntimeState::with_inspector`].
  inspector: RefCell<Option<Rc<RefCell<JsRuntimeInspector>>>>,
  has_inspector: Cell<bool>,
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
  /// the import site. If no callback is provided, all attributes are allowed.
  ///
  /// Embedders might use this callback to eg. validate value of "type"
  /// attribute, not allowing other types than "JSON".
  ///
  /// To signal validation failure, users should throw an V8 exception inside
  /// the callback.
  pub validate_import_attributes_cb: Option<ValidateImportAttributesCb>,

  /// A callback that can be used to customize behavior of
  /// `import.meta.resolve()` API. If no callback is provided, a default one
  /// is used. The default callback returns value of
  /// `RuntimeOptions::module_loader::resolve()` call.
  pub import_meta_resolve_callback: Option<ImportMetaResolveCallback>,

  /// A callback that is called when the event loop has no more work to do,
  /// but there are active, non-blocking inspector session (eg. Chrome
  /// DevTools inspector is connected). The embedder can use this callback
  /// to eg. print a message notifying user about program finished running.
  /// This callback can be called multiple times, eg. after the program finishes
  /// more work can be scheduled from the DevTools.
  pub wait_for_inspector_disconnect_callback:
    Option<WaitForInspectorDisconnectCallback>,

  /// A callback that allows to evaluate a custom type of a module - eg.
  /// embedders might implement loading WASM or test modules.
  pub custom_module_evaluation_cb: Option<CustomModuleEvaluationCb>,
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
      wait_for_inspector: false,
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
    setup::init_v8(v8_platform, cfg!(test), false);
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
    setup::init_v8(v8_platform, cfg!(test), expose_natives);
  }

  /// Only constructor, configuration is done through `options`.
  pub fn new(mut options: RuntimeOptions) -> JsRuntime {
    setup::init_v8(
      options.v8_platform.take(),
      cfg!(test),
      options.unsafe_expose_natives_and_gc(),
    );
    JsRuntime::new_inner(options, false)
  }

  pub(crate) fn state_from(isolate: &v8::Isolate) -> Rc<JsRuntimeState> {
    let state_ptr = isolate.get_data(STATE_DATA_OFFSET);
    let state_rc =
      // SAFETY: We are sure that it's a valid pointer for whole lifetime of
      // the runtime.
      unsafe { Rc::from_raw(state_ptr as *const JsRuntimeState) };
    let state = state_rc.clone();
    std::mem::forget(state_rc);
    state
  }

  /// Returns the `OpState` associated with the passed `Isolate`.
  pub fn op_state_from(isolate: &v8::Isolate) -> Rc<RefCell<OpState>> {
    let state = Self::state_from(isolate);
    state.op_state.clone()
  }

  pub(crate) fn has_more_work(scope: &mut v8::HandleScope) -> bool {
    EventLoopPendingState::new_from_scope(scope).is_pending()
  }

  fn new_inner(mut options: RuntimeOptions, will_snapshot: bool) -> JsRuntime {
    let init_mode = InitMode::from_options(&options);
    let mut op_state = OpState::new(options.feature_checker.take());

    let mut extensions = std::mem::take(&mut options.extensions);
    let mut deno_core_ext = crate::ops_builtin::core::init_ops();
    let op_decls = extension_set::init_ops(&mut deno_core_ext, &mut extensions);
    extension_set::setup_op_state(
      &mut op_state,
      &mut deno_core_ext,
      &mut extensions,
    );
    let (
      global_template_middlewares,
      global_object_middlewares,
      additional_references,
    ) = extension_set::get_middlewares_and_external_refs(&mut extensions);

    let isolate_ptr = setup::create_isolate_ptr();

    let waker = op_state.waker.clone();
    let op_state = Rc::new(RefCell::new(op_state));
    let state_rc = Rc::new(JsRuntimeState {
      source_map_getter: options.source_map_getter.map(Rc::new),
      source_map_cache: Default::default(),
      shared_array_buffer_store: options.shared_array_buffer_store,
      compiled_wasm_module_store: options.compiled_wasm_module_store,
      wait_for_inspector_disconnect_callback: options
        .wait_for_inspector_disconnect_callback,
      op_state: op_state.clone(),
      validate_import_attributes_cb: options.validate_import_attributes_cb,
      custom_module_evaluation_cb: options.custom_module_evaluation_cb,
      waker,
      // Some fields are initialized later after isolate is created
      inspector: None.into(),
      has_inspector: false.into(),
    });

    let op_driver = Rc::new(DefaultOpDriver::default());
    let op_metrics_factory_fn = options.op_metrics_factory_fn.take();
    let get_error_class_fn = options.get_error_class_fn.unwrap_or(&|_| "Error");

    let mut op_ctxs = extension_set::create_op_ctxs(
      op_decls,
      op_metrics_factory_fn,
      op_driver.clone(),
      op_state.clone(),
      state_rc.clone(),
      get_error_class_fn,
    );

    let external_refs =
      bindings::create_external_references(&op_ctxs, &additional_references);

    let mut isolate = setup::create_isolate(
      will_snapshot,
      options.create_params.take(),
      options.startup_snapshot.take(),
      external_refs,
    );

    let cpp_heap = setup::init_cppgc(&mut isolate);

    for op_ctx in op_ctxs.iter_mut() {
      op_ctx.isolate = isolate.as_mut() as *mut Isolate;
    }

    let context_state = Rc::new(ContextState::new(
      op_driver.clone(),
      isolate_ptr,
      options.get_error_class_fn.unwrap_or(&|_| "Error"),
      op_ctxs,
    ));

    // TODO(bartlomieju): factor out
    // Add the task spawners to the OpState
    let spawner = context_state
      .task_spawner_factory
      .clone()
      .new_same_thread_spawner();
    op_state.borrow_mut().put(spawner);
    let spawner = context_state
      .task_spawner_factory
      .clone()
      .new_cross_thread_spawner();
    op_state.borrow_mut().put(spawner);

    // SAFETY: this is first use of `isolate_ptr` so we are sure we're
    // not overwriting an existing pointer.
    isolate = unsafe {
      isolate_ptr.write(isolate);
      isolate_ptr.read()
    };
    op_state.borrow_mut().put(isolate_ptr);

    // TODO(bartlomieju): this context creation and setup is really non-trivial.
    // Additionally it's unclear what is done when snapshotting/running from snapshot.
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

    let mut context_scope: v8::HandleScope =
      v8::HandleScope::with_context(&mut isolate, &main_context);
    let scope = &mut context_scope;
    let context = v8::Local::new(scope, &main_context);

    bindings::initialize_deno_core_namespace(scope, context, init_mode);
    bindings::initialize_context(
      scope,
      context,
      &context_state.op_ctxs,
      init_mode,
    );

    context.set_slot(scope, context_state.clone());

    let inspector = if options.inspector {
      Some(JsRuntimeInspector::new(scope, context, options.is_main))
    } else {
      None
    };

    let loader = options
      .module_loader
      .unwrap_or_else(|| Rc::new(NoopModuleLoader));
    let import_meta_resolve_cb = options
      .import_meta_resolve_callback
      .unwrap_or_else(|| Box::new(default_import_meta_resolve_cb));

    let exception_state = context_state.exception_state.clone();
    let module_map = if let Some(snapshotted_data) = snapshotted_data {
      *exception_state.js_handled_promise_rejection_cb.borrow_mut() =
        snapshotted_data.js_handled_promise_rejection_cb;
      let module_map_snapshotted_data = ModuleMapSnapshottedData {
        module_map_data: snapshotted_data.module_map_data,
        module_handles: snapshotted_data.module_handles,
      };
      Rc::new(ModuleMap::new_from_snapshotted_data(
        loader,
        exception_state,
        import_meta_resolve_cb,
        scope,
        module_map_snapshotted_data,
      ))
    } else {
      Rc::new(ModuleMap::new(
        loader,
        exception_state,
        import_meta_resolve_cb,
      ))
    };

    context.set_slot(scope, module_map.clone());

    let main_realm = {
      let main_realm =
        JsRealmInner::new(context_state, main_context, module_map.clone());
      // TODO(bartlomieju): why is this done in here? Maybe we can hoist it out?
      state_rc.has_inspector.set(inspector.is_some());
      *state_rc.inspector.borrow_mut() = inspector;
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
        main_realm: ManuallyDrop::new(main_realm),
        state: ManuallyDropRc(ManuallyDrop::new(state_rc)),
        v8_isolate: ManuallyDrop::new(isolate),
        cpp_heap: ManuallyDrop::new(cpp_heap),
      },
      init_mode,
      allocations: IsolateAllocations::default(),
      // TODO(bartlomieju): at this point extensions have only JS/ESM sources left;
      // probably worth to add a dedicated struct to represent that.
      extensions: vec![],
      is_main_runtime: options.is_main,
    };

    // TODO(mmastrac): We should thread errors back out of the runtime
    js_runtime
      .init_extension_js(&mut extensions)
      .expect("Failed to evaluate extension JS");

    // TODO(bartlomieju): clean this up - we shouldn't need to store extensions
    // on the `js_runtime` as they are mutable.
    js_runtime.extensions = extensions;

    js_runtime
  }

  #[cfg(test)]
  #[inline]
  pub(crate) fn module_map(&mut self) -> Rc<ModuleMap> {
    self.inner.main_realm.0.module_map()
  }

  #[inline]
  pub fn main_context(&self) -> v8::Global<v8::Context> {
    self.inner.main_realm.0.context().clone()
  }

  #[cfg(test)]
  pub(crate) fn main_realm(&self) -> JsRealm {
    JsRealm::clone(&self.inner.main_realm)
  }

  #[inline]
  pub fn v8_isolate(&mut self) -> &mut v8::OwnedIsolate {
    &mut self.inner.v8_isolate
  }

  #[inline]
  fn v8_isolate_ptr(&mut self) -> *mut v8::Isolate {
    &mut **self.inner.v8_isolate as _
  }

  #[inline]
  pub fn inspector(&mut self) -> Rc<RefCell<JsRuntimeInspector>> {
    self.inner.state.inspector()
  }

  #[inline]
  pub fn wait_for_inspector_disconnect(&mut self) {
    if let Some(callback) = self
      .inner
      .state
      .wait_for_inspector_disconnect_callback
      .as_ref()
    {
      callback();
    }
  }

  pub fn runtime_activity_stats_factory(&self) -> RuntimeActivityStatsFactory {
    RuntimeActivityStatsFactory {
      context_state: self.inner.main_realm.0.context_state.clone(),
      op_state: self.inner.state.op_state.clone(),
    }
  }

  // TODO(bartlomieju): this method probably be public to the crate
  /// Returns the extensions that this runtime is using (including internal ones).
  pub fn extensions(&self) -> &Vec<Extension> {
    &self.extensions
  }

  #[inline]
  pub fn handle_scope(&mut self) -> v8::HandleScope {
    let isolate = &mut self.inner.v8_isolate;
    self.inner.main_realm.handle_scope(isolate)
  }

  #[inline(always)]
  /// Create a scope on the stack with the given context
  fn with_context_scope<'s, T>(
    isolate: *mut v8::Isolate,
    context: *mut v8::Context,
    f: impl FnOnce(&mut v8::HandleScope<'s>) -> T,
  ) -> T {
    // SAFETY: We know this isolate is valid and non-null at this time
    let mut isolate_scope =
      v8::HandleScope::new(unsafe { isolate.as_mut().unwrap_unchecked() });
    // SAFETY: We know the context is valid and non-null at this time, and that a Local and pointer share the
    // same representation
    let context = unsafe { std::mem::transmute(context) };
    let mut scope = v8::ContextScope::new(&mut isolate_scope, context);
    f(&mut scope)
  }

  /// Create a synthetic module - `ext:core/ops` - that exports all ops registered
  /// with the runtime.
  fn execute_virtual_ops_module(
    &mut self,
    context_global: &v8::Global<v8::Context>,
    module_map: Rc<ModuleMap>,
  ) {
    let scope = &mut self.handle_scope();
    let context_local = v8::Local::new(scope, context_global);
    let context_state = JsRealm::state_from_scope(scope);
    let global = context_local.global(scope);
    let synthetic_module_exports = create_exports_for_ops_virtual_module(
      &context_state.op_ctxs,
      scope,
      global,
    );
    let mod_id = module_map
      .new_synthetic_module(
        scope,
        FastString::StaticAscii(VIRTUAL_OPS_MODULE_NAME),
        crate::ModuleType::JavaScript,
        synthetic_module_exports,
      )
      .unwrap();
    module_map.instantiate_module(scope, mod_id).unwrap();
    let mut receiver = module_map.mod_evaluate(scope, mod_id);
    let Poll::Ready(result) =
      receiver.poll_unpin(&mut Context::from_waker(noop_waker_ref()))
    else {
      unreachable!();
    };
    result.unwrap();
  }

  /// Initializes JS of provided Extensions in the given realm.
  fn init_extension_js(
    &mut self,
    extensions: &mut [Extension],
  ) -> Result<(), Error> {
    // Initialization of JS happens in phases:
    // 1. Iterate through all extensions:
    //  a. Execute all extension "script" JS files
    //  b. Load all extension "module" JS files (but do not execute them yet)
    // 2. Iterate through all extensions:
    //  a. If an extension has a `esm_entry_point`, execute it.
    let realm = JsRealm::clone(&self.inner.main_realm);
    let context_global = realm.context();
    let module_map = realm.0.module_map();
    let loader = module_map.loader.borrow().clone();
    let ext_loader = Rc::new(ExtModuleLoader::new(extensions));
    *module_map.loader.borrow_mut() = ext_loader;

    let mut esm_entrypoints = vec![];

    futures::executor::block_on(async {
      // TODO(bartlomieju): this is somewhat duplicated in `bindings::initialize_context`,
      // but for migration period we need to have ops available in both `Deno.core.ops`
      // as well as have them available in "virtual ops module"
      // if !matches!(
      //   self.init_mode,
      //   InitMode::FromSnapshot {
      //     skip_op_registration: true
      //   }
      // ) {
      if self.init_mode == InitMode::New {
        self.execute_virtual_ops_module(context_global, module_map.clone());
      }

      if self.init_mode == InitMode::New {
        for file_source in &BUILTIN_SOURCES {
          realm.execute_script(
            self.v8_isolate(),
            file_source.specifier,
            file_source.load()?,
          )?;
        }
        for file_source in &BUILTIN_ES_MODULES {
          let mut scope = realm.handle_scope(self.v8_isolate());
          module_map.lazy_load_es_module_from_code(
            &mut scope,
            file_source.specifier,
            file_source.load()?,
          )?;
        }
      }
      self.init_cbs(&realm);

      for extension in extensions {
        // If the extension provides "lazy loaded ES modules" then store them
        // on the ModuleMap.
        module_map
          .add_lazy_loaded_esm_sources(extension.get_lazy_loaded_esm_sources());

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

      // ...then execute all entry points
      for specifier in esm_entrypoints {
        let Some(mod_id) =
          module_map.get_id(specifier, RequestedModuleType::None)
        else {
          bail!("{} not present in the module map", specifier);
        };

        let mut receiver = {
          let isolate = self.v8_isolate();
          let scope = &mut realm.handle_scope(isolate);

          module_map.mod_evaluate(scope, mod_id)
        };

        // After evaluate_pending_module, if the module isn't fully evaluated
        // and the resolver solved, it means the module or one of its imports
        // uses TLA.
        match receiver.poll_unpin(&mut Context::from_waker(noop_waker_ref())) {
          Poll::Ready(result) => {
            result
              .with_context(|| format!("Couldn't execute '{specifier}'"))?;
          }
          Poll::Pending => {
            // Find the TLA location and return it as an error
            let scope = &mut realm.handle_scope(self.v8_isolate());
            let messages = module_map.find_stalled_top_level_await(scope);
            assert!(!messages.is_empty());
            let msg = v8::Local::new(scope, &messages[0]);
            let js_error = JsError::from_v8_message(scope, msg);
            return Err(Error::from(js_error))
              .with_context(|| "Top-level await is not allowed in extensions");
          }
        }
      }

      #[cfg(debug_assertions)]
      {
        let mut scope = realm.handle_scope(self.v8_isolate());
        module_map.check_all_modules_evaluated(&mut scope)?;
      }

      Ok::<_, anyhow::Error>(())
    })?;

    let module_map = realm.0.module_map();
    *module_map.loader.borrow_mut() = loader;

    Ok(())
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
      // TODO(bartlomieju): these probably could be captured from main realm so we don't have to
      // look up them again?
      let deno_str =
        v8::String::new_external_onebyte_static(scope, v8_static_strings::DENO)
          .unwrap();
      let core_str =
        v8::String::new_external_onebyte_static(scope, v8_static_strings::CORE)
          .unwrap();
      let event_loop_tick_str = v8::String::new_external_onebyte_static(
        scope,
        v8_static_strings::EVENT_LOOP_TICK,
      )
      .unwrap();
      let build_custom_error_str = v8::String::new_external_onebyte_static(
        scope,
        v8_static_strings::BUILD_CUSTOM_ERROR,
      )
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
    state_rc
      .js_event_loop_tick_cb
      .borrow_mut()
      .replace(Rc::new(event_loop_tick_cb));
    state_rc
      .exception_state
      .js_build_custom_error_cb
      .borrow_mut()
      .replace(Rc::new(build_custom_error_cb));
  }

  /// Returns the runtime's op state, which can be used to maintain ops
  /// and access resources between op calls.
  pub fn op_state(&mut self) -> Rc<RefCell<OpState>> {
    self.inner.state.op_state.clone()
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
    source_code: ModuleCodeString,
  ) -> Result<v8::Global<v8::Value>, Error> {
    let isolate = &mut self.inner.v8_isolate;
    self
      .inner
      .main_realm
      .execute_script(isolate, name, source_code)
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
    let isolate = &mut self.inner.v8_isolate;
    self.inner.main_realm.execute_script(
      isolate,
      name,
      ModuleCodeString::from_static(source_code),
    )
  }

  /// Call a function and return a future resolving with the return value of the
  /// function. If the function returns a promise, the future will resolve only once the
  /// event loop resolves the underlying promise. If the future rejects, the future will
  /// resolve with the underlying error.
  ///
  /// The event loop must be polled seperately for this future to resolve. If the event loop
  /// is not polled, the future will never make progress.
  pub fn call(
    &mut self,
    function: &v8::Global<v8::Function>,
  ) -> impl Future<Output = Result<v8::Global<v8::Value>, Error>> {
    self.call_with_args(function, &[])
  }

  /// Call a function and returns a future resolving with the return value of the
  /// function. If the function returns a promise, the future will resolve only once the
  /// event loop resolves the underlying promise. If the future rejects, the future will
  /// resolve with the underlying error.
  ///
  /// The event loop must be polled seperately for this future to resolve. If the event loop
  /// is not polled, the future will never make progress.
  pub fn scoped_call(
    scope: &mut v8::HandleScope,
    function: &v8::Global<v8::Function>,
  ) -> impl Future<Output = Result<v8::Global<v8::Value>, Error>> {
    Self::scoped_call_with_args(scope, function, &[])
  }

  /// Call a function and returns a future resolving with the return value of the
  /// function. If the function returns a promise, the future will resolve only once the
  /// event loop resolves the underlying promise. If the future rejects, the future will
  /// resolve with the underlying error.
  ///
  /// The event loop must be polled seperately for this future to resolve. If the event loop
  /// is not polled, the future will never make progress.
  pub fn call_with_args(
    &mut self,
    function: &v8::Global<v8::Function>,
    args: &[v8::Global<v8::Value>],
  ) -> impl Future<Output = Result<v8::Global<v8::Value>, Error>> {
    let scope = &mut self.handle_scope();
    Self::scoped_call_with_args(scope, function, args)
  }

  /// Call a function and returns a future resolving with the return value of the
  /// function. If the function returns a promise, the future will resolve only once the
  /// event loop resolves the underlying promise. If the future rejects, the future will
  /// resolve with the underlying error.
  ///
  /// The event loop must be polled seperately for this future to resolve. If the event loop
  /// is not polled, the future will never make progress.
  pub fn scoped_call_with_args(
    scope: &mut v8::HandleScope,
    function: &v8::Global<v8::Function>,
    args: &[v8::Global<v8::Value>],
  ) -> impl Future<Output = Result<v8::Global<v8::Value>, Error>> {
    let scope = &mut v8::TryCatch::new(scope);
    let cb = function.open(scope);
    let this = v8::undefined(scope).into();
    let promise = if args.is_empty() {
      cb.call(scope, this, &[])
    } else {
      let mut local_args: SmallVec<[v8::Local<v8::Value>; 8]> =
        SmallVec::with_capacity(args.len());
      for v in args {
        local_args.push(v8::Local::new(scope, v));
      }
      cb.call(scope, this, &local_args)
    };

    if promise.is_none() {
      if scope.is_execution_terminating() {
        let undefined = v8::undefined(scope).into();
        return RcPromiseFuture::new(exception_to_err_result(
          scope, undefined, false, true,
        ));
      }
      let exception = scope.exception().unwrap();
      return RcPromiseFuture::new(exception_to_err_result(
        scope, exception, false, true,
      ));
    }
    let promise = promise.unwrap();
    if !promise.is_promise() {
      return RcPromiseFuture::new(Ok(v8::Global::new(scope, promise)));
    }
    let promise = v8::Local::<v8::Promise>::try_from(promise).unwrap();
    Self::resolve_promise_inner(scope, promise)
  }

  /// Call a function. If it returns a promise, run the event loop until that
  /// promise is settled. If the promise rejects or there is an uncaught error
  /// in the event loop, return `Err(error)`. Or return `Ok(<await returned>)`.
  #[deprecated = "Use call"]
  pub async fn call_and_await(
    &mut self,
    function: &v8::Global<v8::Function>,
  ) -> Result<v8::Global<v8::Value>, Error> {
    let call = self.call(function);
    self
      .with_event_loop_promise(call, PollEventLoopOptions::default())
      .await
  }

  /// Call a function with args. If it returns a promise, run the event loop until that
  /// promise is settled. If the promise rejects or there is an uncaught error
  /// in the event loop, return `Err(error)`. Or return `Ok(<await returned>)`.
  #[deprecated = "Use call_with_args"]
  pub async fn call_with_args_and_await(
    &mut self,
    function: &v8::Global<v8::Function>,
    args: &[v8::Global<v8::Value>],
  ) -> Result<v8::Global<v8::Value>, Error> {
    let call = self.call_with_args(function, args);
    self
      .with_event_loop_promise(call, PollEventLoopOptions::default())
      .await
  }

  /// Returns the namespace object of a module.
  ///
  /// This is only available after module evaluation has completed.
  /// This function panics if module has not been instantiated.
  pub fn get_module_namespace(
    &mut self,
    module_id: ModuleId,
  ) -> Result<v8::Global<v8::Object>, Error> {
    let isolate = &mut self.inner.v8_isolate;
    self
      .inner
      .main_realm
      .get_module_namespace(isolate, module_id)
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
      Some(exception) => {
        exception_to_err_result(tc_scope, exception, false, true)
      }
    }
  }

  pub fn maybe_init_inspector(&mut self) {
    let inspector = &mut self.inner.state.inspector.borrow_mut();
    if inspector.is_some() {
      return;
    }

    let context = self.main_context();
    let scope = &mut v8::HandleScope::with_context(
      self.inner.v8_isolate.as_mut(),
      context.clone(),
    );
    let context = v8::Local::new(scope, context);

    self.inner.state.has_inspector.set(true);
    **inspector = Some(JsRuntimeInspector::new(
      scope,
      context,
      self.is_main_runtime,
    ));
  }

  /// Waits for the given value to resolve while polling the event loop.
  ///
  /// This future resolves when either the value is resolved or the event loop runs to
  /// completion.
  pub fn resolve(
    &mut self,
    promise: v8::Global<v8::Value>,
  ) -> impl Future<Output = Result<v8::Global<v8::Value>, Error>> {
    let scope = &mut self.handle_scope();
    Self::scoped_resolve(scope, promise)
  }

  /// Waits for the given value to resolve while polling the event loop.
  ///
  /// This future resolves when either the value is resolved or the event loop runs to
  /// completion.
  pub fn scoped_resolve(
    scope: &mut v8::HandleScope,
    promise: v8::Global<v8::Value>,
  ) -> impl Future<Output = Result<v8::Global<v8::Value>, Error>> {
    let promise = v8::Local::new(scope, promise);
    if !promise.is_promise() {
      return RcPromiseFuture::new(Ok(v8::Global::new(scope, promise)));
    }
    let promise = v8::Local::<v8::Promise>::try_from(promise).unwrap();
    Self::resolve_promise_inner(scope, promise)
  }

  /// Waits for the given value to resolve while polling the event loop.
  ///
  /// This future resolves when either the value is resolved or the event loop runs to
  /// completion.
  #[deprecated = "Use resolve"]
  pub async fn resolve_value(
    &mut self,
    global: v8::Global<v8::Value>,
  ) -> Result<v8::Global<v8::Value>, Error> {
    let resolve = self.resolve(global);
    self
      .with_event_loop_promise(resolve, PollEventLoopOptions::default())
      .await
  }

  /// Given a promise, returns a future that resolves when it does.
  fn resolve_promise_inner<'s>(
    scope: &mut v8::HandleScope<'s>,
    promise: v8::Local<'s, v8::Promise>,
  ) -> RcPromiseFuture {
    let future = RcPromiseFuture::default();
    let f = future.clone();
    watch_promise(scope, promise, move |scope, _rv, res| {
      let res = match res {
        Ok(l) => Ok(v8::Global::new(scope, l)),
        Err(e) => exception_to_err_result(scope, e, true, true),
      };
      f.0.resolved.set(Some(res));
      if let Some(waker) = f.0.waker.take() {
        waker.wake();
      }
    });

    future
  }

  /// Runs event loop to completion
  ///
  /// This future resolves when:
  ///  - there are no more pending dynamic imports
  ///  - there are no more pending ops
  ///  - there are no more active inspector sessions (only if
  ///     `PollEventLoopOptions.wait_for_inspector` is set to true)
  pub async fn run_event_loop(
    &mut self,
    poll_options: PollEventLoopOptions,
  ) -> Result<(), Error> {
    poll_fn(|cx| self.poll_event_loop(cx, poll_options)).await
  }

  /// A utility function that run provided future concurrently with the event loop.
  ///
  /// If the event loop resolves while polling the future, it return an error with the text
  /// `Promise resolution is still pending but the event loop has already resolved.`
  pub async fn with_event_loop_promise<'fut, T, E>(
    &mut self,
    mut fut: impl Future<Output = Result<T, E>> + Unpin + 'fut,
    poll_options: PollEventLoopOptions,
  ) -> Result<T, AnyError>
  where
    AnyError: From<E>,
  {
    // Manually implement tokio::select
    poll_fn(|cx| {
      if let Poll::Ready(t) = fut.poll_unpin(cx) {
        return Poll::Ready(t.map_err(|e| e.into()));
      }
      if let Poll::Ready(t) = self.poll_event_loop(cx, poll_options) {
        t?;
        if let Poll::Ready(t) = fut.poll_unpin(cx) {
          return Poll::Ready(t.map_err(|e| e.into()));
        }
        return Poll::Ready(Err(anyhow!("Promise resolution is still pending but the event loop has already resolved.")));
      }
      Poll::Pending
    }).await
  }

  /// A utility function that run provided future concurrently with the event loop.
  ///
  /// If the event loop resolves while polling the future, it will continue to be polled,
  /// regardless of whether it returned an error or success.
  ///
  /// Useful for interacting with local inspector session.
  pub async fn with_event_loop_future<'fut, T, E>(
    &mut self,
    mut fut: impl Future<Output = Result<T, E>> + Unpin + 'fut,
    poll_options: PollEventLoopOptions,
  ) -> Result<T, AnyError>
  where
    AnyError: From<E>,
  {
    // Manually implement tokio::select
    poll_fn(|cx| {
      if let Poll::Ready(t) = fut.poll_unpin(cx) {
        return Poll::Ready(t.map_err(|e| e.into()));
      }
      if let Poll::Ready(t) = self.poll_event_loop(cx, poll_options) {
        // TODO(mmastrac): We need to ignore this error for things like the repl to behave as
        // they did before, but this is definitely not correct. It's just something we're
        // relying on. :(
        _ = t;
      }
      Poll::Pending
    })
    .await
  }

  /// Runs a single tick of event loop
  ///
  /// If `PollEventLoopOptions.wait_for_inspector` is set to true, the event
  /// loop will return `Poll::Pending` if there are active inspector sessions.
  pub fn poll_event_loop(
    &mut self,
    cx: &mut Context,
    poll_options: PollEventLoopOptions,
  ) -> Poll<Result<(), Error>> {
    let isolate = self.v8_isolate_ptr();
    Self::with_context_scope(
      isolate,
      self.inner.main_realm.context_ptr(),
      move |scope| self.poll_event_loop_inner(cx, scope, poll_options),
    )
  }

  fn poll_event_loop_inner(
    &mut self,
    cx: &mut Context,
    scope: &mut v8::HandleScope,
    poll_options: PollEventLoopOptions,
  ) -> Poll<Result<(), Error>> {
    let has_inspector = self.inner.state.has_inspector.get();
    self.inner.state.waker.register(cx.waker());

    if has_inspector {
      // We poll the inspector first.
      let _ = self.inspector().borrow().poll_sessions(Some(cx)).unwrap();
    }

    if poll_options.pump_v8_message_loop {
      self.pump_v8_message_loop(scope)?;
    }

    let realm = &self.inner.main_realm;
    let modules = &realm.0.module_map;
    let context_state = &realm.0.context_state;
    let exception_state = &context_state.exception_state;

    modules.poll_progress(cx, scope)?;

    // Resolve async ops, run all next tick callbacks and macrotasks callbacks
    // and only then check for any promise exceptions (`unhandledrejection`
    // handlers are run in macrotasks callbacks so we need to let them run
    // first).
    let dispatched_ops = Self::do_js_event_loop_tick_realm(
      cx,
      scope,
      context_state,
      exception_state,
    )?;
    exception_state.check_exception_condition(scope)?;

    // Get the pending state from the main realm, or all realms
    let pending_state =
      EventLoopPendingState::new(scope, context_state, modules);

    if !pending_state.is_pending() {
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
            inspector.borrow_mut().context_destroyed(scope, context);
            self.wait_for_inspector_disconnect();
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
        || pending_state.has_pending_promise_events
      {
        self.inner.state.waker.wake();
      } else
      // If ops were dispatched we may have progress on pending modules that we should re-check
      if (pending_state.has_pending_module_evaluation
        || pending_state.has_pending_dyn_module_evaluation)
        && dispatched_ops
      {
        self.inner.state.waker.wake();
      }
    }

    if pending_state.has_pending_module_evaluation {
      if pending_state.has_pending_refed_ops
        || pending_state.has_pending_dyn_imports
        || pending_state.has_pending_dyn_module_evaluation
        || pending_state.has_pending_background_tasks
        || pending_state.has_tick_scheduled
      {
        // pass, will be polled again
      } else {
        return Poll::Ready(Err(
          find_and_report_stalled_level_await_in_any_realm(scope, &realm.0),
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
      } else if realm.modules_idle() {
        return Poll::Ready(Err(
          find_and_report_stalled_level_await_in_any_realm(scope, &realm.0),
        ));
      } else {
        // Delay the above error by one spin of the event loop. A dynamic import
        // evaluation may complete during this, in which case the counter will
        // reset.
        realm.increment_modules_idle();
        self.inner.state.waker.wake();
      }
    }

    Poll::Pending
  }
}

fn find_and_report_stalled_level_await_in_any_realm(
  scope: &mut v8::HandleScope,
  inner_realm: &JsRealmInner,
) -> Error {
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
    setup::init_v8(
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

    let realm = JsRealm::clone(&self.inner.main_realm);

    // Set the context to be snapshot's default context
    {
      let mut scope = realm.handle_scope(self.v8_isolate());
      let local_context = v8::Local::new(&mut scope, realm.context());
      scope.set_default_context(local_context);
    }

    // Serialize the module map and store its data in the snapshot.
    {
      let module_map_snapshotted_data = {
        let module_map = realm.0.module_map();
        module_map.serialize_for_snapshotting(
          &mut realm.handle_scope(self.v8_isolate()),
        )
      };
      let maybe_js_handled_promise_rejection_cb = {
        let context_state = &realm.0.context_state;
        let exception_state = &context_state.exception_state;
        exception_state
          .js_handled_promise_rejection_cb
          .borrow()
          .clone()
      };

      let snapshotted_data = SnapshottedData {
        module_map_data: module_map_snapshotted_data.module_map_data,
        module_handles: module_map_snapshotted_data.module_handles,
        js_handled_promise_rejection_cb: maybe_js_handled_promise_rejection_cb,
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
  has_pending_refed_ops: bool,
  has_pending_dyn_imports: bool,
  has_pending_dyn_module_evaluation: bool,
  has_pending_module_evaluation: bool,
  has_pending_background_tasks: bool,
  has_tick_scheduled: bool,
  has_pending_promise_events: bool,
}

impl EventLoopPendingState {
  /// Collect event loop state from all the sub-states.
  pub fn new(
    scope: &mut v8::HandleScope<()>,
    state: &ContextState,
    modules: &ModuleMap,
  ) -> Self {
    let num_unrefed_ops = state.unrefed_ops.borrow().len();
    let num_pending_ops = state.pending_ops.len();
    let has_pending_tasks = state.task_spawner_factory.has_pending_tasks();
    let has_pending_timers = state.timers.has_pending_timers();
    let has_pending_dyn_imports = modules.has_pending_dynamic_imports();
    let has_pending_dyn_module_evaluation =
      modules.has_pending_dyn_module_evaluation();
    let has_pending_module_evaluation = modules.has_pending_module_evaluation();
    let has_pending_promise_events = !state
      .exception_state
      .pending_promise_rejections
      .borrow()
      .is_empty()
      || !state
        .exception_state
        .pending_handled_promise_rejections
        .borrow()
        .is_empty();
    EventLoopPendingState {
      has_pending_refed_ops: has_pending_tasks
        || has_pending_timers
        || num_pending_ops > num_unrefed_ops,
      has_pending_dyn_imports,
      has_pending_dyn_module_evaluation,
      has_pending_module_evaluation,
      has_pending_background_tasks: scope.has_pending_background_tasks(),
      has_tick_scheduled: state.has_next_tick_scheduled.get(),
      has_pending_promise_events,
    }
  }

  /// Collect event loop state from all the states stored in the scope.
  pub fn new_from_scope(scope: &mut v8::HandleScope) -> Self {
    let module_map = JsRealm::module_map_from(scope);
    let context_state = JsRealm::state_from_scope(scope);
    Self::new(scope, &context_state, &module_map)
  }

  pub fn is_pending(&self) -> bool {
    self.has_pending_refed_ops
      || self.has_pending_dyn_imports
      || self.has_pending_dyn_module_evaluation
      || self.has_pending_module_evaluation
      || self.has_pending_background_tasks
      || self.has_tick_scheduled
      || self.has_pending_promise_events
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
    self.inspector.borrow().as_ref().unwrap().clone()
  }

  /// Called by `bindings::host_import_module_dynamically_callback`
  /// after initiating new dynamic import load.
  pub fn notify_new_dynamic_import(&self) {
    // Notify event loop to poll again soon.
    self.waker.wake();
  }

  /// Performs an action with the inspector, if we have one
  pub(crate) fn with_inspector<T>(
    &self,
    mut f: impl FnMut(&JsRuntimeInspector) -> T,
  ) -> Option<T> {
    // Fast path
    if !self.has_inspector.get() {
      return None;
    }
    self
      .inspector
      .borrow()
      .as_ref()
      .map(|inspector| f(&inspector.borrow()))
  }
}

// Related to module loading
impl JsRuntime {
  #[cfg(test)]
  pub(crate) fn instantiate_module(
    &mut self,
    id: ModuleId,
  ) -> Result<(), v8::Global<v8::Value>> {
    let isolate = &mut self.inner.v8_isolate;
    let realm = JsRealm::clone(&self.inner.main_realm);
    let scope = &mut realm.handle_scope(isolate);
    realm.instantiate_module(scope, id)
  }

  /// Evaluates an already instantiated ES module.
  ///
  /// Returns a future that resolves when module promise resolves.
  /// Implementors must manually call [`JsRuntime::run_event_loop`] to drive
  /// module evaluation future.
  ///
  /// Modules with top-level await are treated like promises, so a `throw` in the top-level
  /// block of a module is treated as an unhandled rejection. These rejections are provided to
  /// the unhandled promise rejection handler, which has the opportunity to pass them off to
  /// error-handling code. If those rejections are not handled (indicated by a `false` return
  /// from that unhandled promise rejection handler), then the runtime will terminate.
  ///
  /// The future provided by `mod_evaluate` will only return errors in the case where
  /// the runtime is shutdown and no longer available to provide unhandled rejection
  /// information.
  ///
  /// This function panics if module has not been instantiated.
  pub fn mod_evaluate(
    &mut self,
    id: ModuleId,
  ) -> impl Future<Output = Result<(), Error>> {
    let isolate = &mut self.inner.v8_isolate;
    let realm = &self.inner.main_realm;
    let scope = &mut realm.handle_scope(isolate);
    self.inner.main_realm.0.module_map.mod_evaluate(scope, id)
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
    code: Option<ModuleCodeString>,
  ) -> Result<ModuleId, Error> {
    let isolate = &mut self.inner.v8_isolate;
    self
      .inner
      .main_realm
      .load_main_module(isolate, specifier, code)
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
    code: Option<ModuleCodeString>,
  ) -> Result<ModuleId, Error> {
    let isolate = &mut self.inner.v8_isolate;
    self
      .inner
      .main_realm
      .load_side_module(isolate, specifier, code)
      .await
  }

  /// Load and evaluate an ES module provided the specifier and source code.
  ///
  /// The module should not have Top-Level Await (that is, it should be
  /// possible to evaluate it synchronously).
  ///
  /// It is caller's responsibility to ensure that not duplicate specifiers are
  /// passed to this method.
  pub fn lazy_load_es_module_from_code(
    &mut self,
    specifier: &str,
    code: ModuleCodeString,
  ) -> Result<v8::Global<v8::Value>, Error> {
    let isolate = &mut self.inner.v8_isolate;
    self
      .inner
      .main_realm
      .lazy_load_es_module_from_code(isolate, specifier, code)
  }

  fn do_js_event_loop_tick_realm(
    cx: &mut Context,
    scope: &mut v8::HandleScope,
    context_state: &ContextState,
    exception_state: &ExceptionState,
  ) -> Result<bool, Error> {
    let mut dispatched_ops = false;

    // Poll any pending task spawner tasks. Note that we need to poll separately because otherwise
    // Rust will extend the lifetime of the borrow longer than we expect.
    let tasks = context_state.task_spawner_factory.poll_inner(cx);
    if let Poll::Ready(tasks) = tasks {
      // TODO(mmastrac): we are using this flag
      dispatched_ops = true;
      for task in tasks {
        task(scope);
      }
    }

    // We return async responses to JS in bounded batches. Note that because
    // we're passing these to JS as arguments, it is possible to overflow the
    // JS stack by just passing too many.
    const MAX_VEC_SIZE_FOR_OPS: usize = 1024;

    // each batch is a flat vector of tuples:
    // `[promise_id1, op_result1, promise_id2, op_result2, ...]`
    // promise_id is a simple integer, op_result is an ops::OpResult
    // which contains a value OR an error, encoded as a tuple.
    // This batch is received in JS via the special `arguments` variable
    // and then each tuple is used to resolve or reject promises
    let mut args: SmallVec<[v8::Local<v8::Value>; 32]> =
      SmallVec::with_capacity(32);

    loop {
      if args.len() >= MAX_VEC_SIZE_FOR_OPS {
        // We have too many, bail for now but re-wake the waker
        cx.waker().wake_by_ref();
        break;
      }

      let Poll::Ready((promise_id, op_id, res)) =
        context_state.pending_ops.poll_ready(cx)
      else {
        break;
      };

      let res = res.unwrap(scope, context_state.get_error_class_fn);

      {
        let op_ctx = &context_state.op_ctxs[op_id as usize];
        if op_ctx.metrics_enabled() {
          if res.is_ok() {
            dispatch_metrics_async(op_ctx, OpMetricsEvent::CompletedAsync);
          } else {
            dispatch_metrics_async(op_ctx, OpMetricsEvent::ErrorAsync);
          }
        }
      }

      context_state.unrefed_ops.borrow_mut().remove(&promise_id);
      dispatched_ops |= true;
      args.push(v8::Integer::new(scope, promise_id).into());
      args.push(res.unwrap_or_else(std::convert::identity));
    }

    let undefined: v8::Local<v8::Value> = v8::undefined(scope).into();
    let has_tick_scheduled = context_state.has_next_tick_scheduled.get();
    dispatched_ops |= has_tick_scheduled;

    while let Some((promise, result)) = exception_state
      .pending_handled_promise_rejections
      .borrow_mut()
      .pop_front()
    {
      if let Some(handler) = exception_state
        .js_handled_promise_rejection_cb
        .borrow()
        .as_ref()
      {
        let function = handler.open(scope);

        let args = [
          v8::Local::new(scope, promise).into(),
          v8::Local::new(scope, result),
        ];
        function.call(scope, undefined, &args);
      }
    }

    let rejections = if !exception_state
      .pending_promise_rejections
      .borrow_mut()
      .is_empty()
    {
      let mut rejections =
        exception_state.pending_promise_rejections.borrow_mut();
      let arr = v8::Array::new(scope, (rejections.len() * 2) as i32);
      let mut index = 0;
      for rejection in rejections.drain(..) {
        let value = v8::Local::new(scope, rejection.0);
        arr.set_index(scope, index, value.into());
        index += 1;
        let value = v8::Local::new(scope, rejection.1);
        arr.set_index(scope, index, value);
        index += 1;
      }
      drop(rejections);
      arr.into()
    } else {
      undefined
    };

    args.push(rejections);

    let timers =
      if let Poll::Ready(timers) = context_state.timers.poll_timers(cx) {
        let arr = v8::Array::new(scope, (timers.len() * 2) as _);
        #[allow(clippy::needless_range_loop)]
        for i in 0..timers.len() {
          let value = v8::Integer::new(scope, timers[i].1 .1 as _);
          arr.set_index(scope, (i * 2) as _, value.into());
          let value = v8::Local::new(scope, timers[i].1 .0.clone());
          arr.set_index(scope, (i * 2 + 1) as _, value.into());
        }
        arr.into()
      } else {
        undefined
      };
    args.push(timers);

    let has_tick_scheduled = v8::Boolean::new(scope, has_tick_scheduled);
    args.push(has_tick_scheduled.into());

    let tc_scope = &mut v8::TryCatch::new(scope);
    let js_event_loop_tick_cb = context_state.js_event_loop_tick_cb.borrow();
    let js_event_loop_tick_cb =
      js_event_loop_tick_cb.as_ref().unwrap().open(tc_scope);

    js_event_loop_tick_cb.call(tc_scope, undefined, args.as_slice());

    if let Some(exception) = tc_scope.exception() {
      return exception_to_err_result(tc_scope, exception, false, true);
    }

    if tc_scope.has_terminated() || tc_scope.is_execution_terminating() {
      return Ok(false);
    }

    Ok(dispatched_ops)
  }
}
