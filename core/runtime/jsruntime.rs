// Copyright 2018-2025 the Deno authors. MIT license.

use super::SnapshotStoreDataStore;
use super::SnapshottedData;
use super::bindings;
use super::bindings::create_exports_for_ops_virtual_module;
use super::bindings::watch_promise;
use super::exception_state::ExceptionState;
use super::jsrealm::JsRealmInner;
use super::op_driver::OpDriver;
use super::setup;
use super::snapshot;
use super::stats::RuntimeActivityStatsFactory;
use super::v8_static_strings::*;
use crate::Extension;
use crate::ExtensionArguments;
use crate::ExtensionFileSource;
use crate::ExtensionFileSourceCode;
use crate::FastStaticString;
use crate::FastString;
use crate::ModuleCodeString;
use crate::NoopModuleLoader;
use crate::OpMetadata;
use crate::OpMetricsEvent;
use crate::OpStackTraceCallback;
use crate::OpState;
use crate::ascii_str;
use crate::ascii_str_include;
use crate::cppgc::FunctionTemplateData;
use crate::error::CoreError;
use crate::error::CoreErrorKind;
use crate::error::CoreModuleExecuteError;
use crate::error::CoreModuleParseError;
use crate::error::ExtensionLazyInitCountMismatchError;
use crate::error::ExtensionLazyInitOrderMismatchError;
use crate::error::JsError;
use crate::error::exception_to_err_result;
use crate::extension_set;
use crate::extension_set::LoadedSources;
use crate::extensions::GlobalObjectMiddlewareFn;
use crate::extensions::GlobalTemplateMiddlewareFn;
use crate::inspector::JsRuntimeInspector;
use crate::module_specifier::ModuleSpecifier;
use crate::modules::CustomModuleEvaluationCb;
use crate::modules::EvalContextCodeCacheReadyCb;
use crate::modules::EvalContextGetCodeCacheCb;
use crate::modules::ExtCodeCache;
use crate::modules::ExtModuleLoader;
use crate::modules::IntoModuleCodeString;
use crate::modules::IntoModuleName;
use crate::modules::ModuleId;
use crate::modules::ModuleLoader;
use crate::modules::ModuleMap;
use crate::modules::ModuleName;
use crate::modules::RequestedModuleType;
use crate::modules::ValidateImportAttributesCb;
use crate::modules::script_origin;
use crate::ops_metrics::OpMetricsFactoryFn;
use crate::ops_metrics::dispatch_metrics_async;
use crate::runtime::ContextState;
use crate::runtime::JsRealm;
use crate::runtime::OpDriverImpl;
use crate::source_map::SourceMapData;
use crate::source_map::SourceMapper;
use crate::stats::RuntimeActivityType;
use deno_error::JsErrorBox;
use futures::FutureExt;
use futures::task::AtomicWaker;
use smallvec::SmallVec;
use std::any::Any;
use std::future::Future;
use std::future::poll_fn;
use v8::MessageErrorLevel;

use std::cell::Cell;
use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::ffi::c_void;
use std::mem::ManuallyDrop;
use std::ops::Deref;
use std::ops::DerefMut;
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

pub type ExtensionTranspiler =
  dyn Fn(
    ModuleName,
    ModuleCodeString,
  ) -> Result<(ModuleCodeString, Option<SourceMapData>), JsErrorBox>;

/// Objects that need to live as long as the isolate
#[derive(Default)]
pub(crate) struct IsolateAllocations {
  pub(crate) externalized_sources: Box<[v8::OneByteConst]>,
  pub(crate) original_sources: Box<[FastString]>,
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
  extensions: Vec<&'static str>,
  op_count: usize,
  source_count: usize,
  addl_refs_count: usize,
  main_realm: ManuallyDrop<JsRealm>,
  pub(crate) state: ManuallyDropRc<JsRuntimeState>,
  v8_isolate: ManuallyDrop<v8::OwnedIsolate>,
}

impl InnerIsolateState {
  /// Clean out the opstate and take the inspector to prevent the inspector from getting destroyed
  /// after we've torn down the contexts. If the inspector is not correctly torn down, random crashes
  /// happen in tests (and possibly for users using the inspector).
  pub fn prepare_for_cleanup(&mut self) {
    // Explicitly shut down the op driver here, just in case there are other references to it
    // that prevent it from dropping after we invalidate the state.
    self.main_realm.0.context_state.pending_ops.shutdown();
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
    unsafe {
      ManuallyDrop::drop(&mut self.state.0);

      let isolate = ManuallyDrop::take(&mut self.v8_isolate);

      std::mem::forget(self);

      isolate
    }
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
        #[allow(clippy::print_stderr)]
        {
          eprintln!("WARNING: v8::OwnedIsolate for snapshot was leaked");
        }
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
  pub fn needs_ops_bindings(&self) -> bool {
    !matches!(
      self,
      InitMode::FromSnapshot {
        skip_op_registration: true
      }
    )
  }
}

#[derive(Default)]
struct PromiseFuture {
  resolved: Cell<Option<Result<v8::Global<v8::Value>, JsError>>>,
  waker: Cell<Option<Waker>>,
}

#[derive(Clone, Default)]
struct RcPromiseFuture(Rc<PromiseFuture>);

impl RcPromiseFuture {
  pub fn new(res: Result<v8::Global<v8::Value>, JsError>) -> Self {
    Self(Rc::new(PromiseFuture {
      resolved: Some(res).into(),
      ..Default::default()
    }))
  }
}

impl Future for RcPromiseFuture {
  type Output = Result<v8::Global<v8::Value>, JsError>;

  fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
    let this = self.get_mut();
    match this.0.resolved.take() {
      Some(resolved) => Poll::Ready(resolved),
      _ => {
        this.0.waker.set(Some(cx.waker().clone()));
        Poll::Pending
      }
    }
  }
}

static VIRTUAL_OPS_MODULE_NAME: FastStaticString = ascii_str!("ext:core/ops");

pub(crate) struct InternalSourceFile {
  pub specifier: FastStaticString,
  pub source: FastStaticString,
}

macro_rules! internal_source_file {
  ($str_:literal) => {{
    InternalSourceFile {
      specifier: ascii_str!(concat!("ext:core/", $str_)),
      source: ascii_str_include!(concat!("../", $str_)),
    }
  }};
}

/// These files are executed just after a new context is created. They provided
/// the necessary infrastructure to bind ops.
pub(crate) static CONTEXT_SETUP_SOURCES: [InternalSourceFile; 2] = [
  internal_source_file!("00_primordials.js"),
  internal_source_file!("00_infra.js"),
];

/// These files are executed when we start setting up extensions. They rely
/// on ops being already fully set up.
pub(crate) static BUILTIN_SOURCES: [InternalSourceFile; 1] =
  [internal_source_file!("01_core.js")];

/// Executed after `BUILTIN_SOURCES` are executed. Provides a thin ES module
/// that exports `core`, `internals` and `primordials` objects.
pub(crate) static BUILTIN_ES_MODULES: [ExtensionFileSource; 1] =
  [ExtensionFileSource::new(
    "ext:core/mod.js",
    ascii_str_include!("../mod.js"),
  )];

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
  // Contains paths of source files that were executed in
  // [`JsRuntime::init_extension_js`]. This field is populated only if a
  // snapshot is being created.
  files_loaded_from_fs_during_snapshot: Vec<&'static str>,
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
  pub(crate) source_mapper: RefCell<SourceMapper>,
  pub(crate) op_state: Rc<RefCell<OpState>>,
  pub(crate) shared_array_buffer_store: Option<SharedArrayBufferStore>,
  pub(crate) compiled_wasm_module_store: Option<CompiledWasmModuleStore>,
  wait_for_inspector_disconnect_callback:
    Option<WaitForInspectorDisconnectCallback>,
  pub(crate) validate_import_attributes_cb: Option<ValidateImportAttributesCb>,
  pub(crate) custom_module_evaluation_cb: Option<CustomModuleEvaluationCb>,
  pub(crate) eval_context_get_code_cache_cb:
    RefCell<Option<EvalContextGetCodeCacheCb>>,
  pub(crate) eval_context_code_cache_ready_cb:
    RefCell<Option<EvalContextCodeCacheReadyCb>>,
  pub(crate) cppgc_template: RefCell<Option<v8::Global<v8::FunctionTemplate>>>,
  pub(crate) function_templates: Rc<RefCell<FunctionTemplateData>>,
  pub(crate) callsite_prototype: RefCell<Option<v8::Global<v8::Object>>>,
  waker: Arc<AtomicWaker>,
  /// Accessed through [`JsRuntimeState::with_inspector`].
  inspector: RefCell<Option<Rc<RefCell<JsRuntimeInspector>>>>,
  has_inspector: Cell<bool>,
  import_assertions_support: ImportAssertionsSupport,
  lazy_extensions: Vec<&'static str>,
}

#[derive(Default)]
pub struct RuntimeOptions {
  /// Implementation of `ModuleLoader` which will be
  /// called when V8 requests to load ES modules in the main realm.
  ///
  /// If not provided runtime will error if code being
  /// executed tries to load modules.
  pub module_loader: Option<Rc<dyn ModuleLoader>>,

  /// If specified, enables V8 code cache for extension code.
  pub extension_code_cache: Option<Rc<dyn ExtCodeCache>>,

  /// If specified, transpiles extensions before loading.
  pub extension_transpiler: Option<Rc<ExtensionTranspiler>>,

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
  ///
  /// For testing, use `runtime.snapshot()` and then [`Box::leak`] to acquire
  // a static slice.
  pub startup_snapshot: Option<&'static [u8]>,

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

  /// A callback that can be used to validate import attributes received at
  /// the import site. If no callback is provided, all attributes are allowed.
  ///
  /// Embedders might use this callback to eg. validate value of "type"
  /// attribute, not allowing other types than "JSON".
  ///
  /// To signal validation failure, users should throw an V8 exception inside
  /// the callback.
  pub validate_import_attributes_cb: Option<ValidateImportAttributesCb>,

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

  /// Callbacks to retrieve and store code cache for scripts evaluated
  /// through evalContext.
  pub eval_context_code_cache_cbs:
    Option<(EvalContextGetCodeCacheCb, EvalContextCodeCacheReadyCb)>,

  pub import_assertions_support: ImportAssertionsSupport,

  /// A callback to specify how stack traces should be used when an op is
  /// annotated with `stack_trace` attribute. Use wisely, as it's very expensive
  /// to collect stack traces on each op invocation.
  pub maybe_op_stack_trace_callback: Option<OpStackTraceCallback>,
}

pub struct ImportAssertionsSupportCustomCallbackArgs {
  pub maybe_specifier: Option<String>,
  pub maybe_line_number: Option<usize>,
  pub column_number: usize,
  pub maybe_source_line: Option<String>,
}

#[derive(Default)]
pub enum ImportAssertionsSupport {
  /// `assert` keyword is no longer supported and causes a SyntaxError.
  /// This is the default setting, because V8 unshipped import assertions
  /// support in v12.6. To enable import assertion one must pass
  /// `--harmony-import-assertions` V8 flag when initializing the runtime.
  #[default]
  Error,

  /// `assert` keyword is supported.
  Yes,

  /// `assert` keyword is supported, but prints a warning message on occurence.
  Warning,

  /// `assert` keyword is supported, provided callback is called on each occurence.
  /// Callback receives optional specifier, optional line number, column number and optional
  /// source line.
  CustomCallback(Box<dyn Fn(ImportAssertionsSupportCustomCallbackArgs)>),
}

impl ImportAssertionsSupport {
  fn is_enabled(&self) -> bool {
    matches!(self, Self::Yes | Self::Warning | Self::CustomCallback(_))
  }

  fn has_warning(&self) -> bool {
    matches!(self, Self::Warning | Self::CustomCallback(_))
  }
}

/// Currently only handles warnings for "import assertions" deprecation
extern "C" fn isolate_message_listener(
  message: v8::Local<v8::Message>,
  _exception: v8::Local<v8::Value>,
) {
  let scope = &mut unsafe { v8::CallbackScope::new(message) };
  let scope = &mut v8::HandleScope::new(scope);
  let message_v8_str = message.get(scope);
  let message_str = message_v8_str.to_rust_string_lossy(scope);

  if !message_str.starts_with("'assert' is deprecated") {
    return;
  }

  let maybe_script_resource_name = message
    .get_script_resource_name(scope)
    .map(|s| s.to_rust_string_lossy(scope));
  let maybe_source_line = message
    .get_source_line(scope)
    .map(|s| s.to_rust_string_lossy(scope));
  let maybe_line_number = message.get_line_number(scope);
  let start_column = message.get_start_column();

  let js_runtime_state = JsRuntime::state_from(scope);
  if let Some(specifier) = maybe_script_resource_name.as_ref() {
    let module_map = JsRealm::module_map_from(scope);
    module_map
      .loader
      .borrow()
      .purge_and_prevent_code_cache(specifier);
  }

  match &js_runtime_state.import_assertions_support {
    ImportAssertionsSupport::Warning => {
      let mut msg = "⚠️  Import assertions are deprecated. Use `with` keyword, instead of 'assert' keyword.".to_string();
      if let Some(specifier) = maybe_script_resource_name {
        if let Some(source_line) = maybe_source_line {
          msg.push('\n');
          msg.push_str(&source_line);
          msg.push('\n');
          msg.push_str(&format!("{:0width$}^", " ", width = start_column));
        }
        msg.push_str(&format!(
          "  at {}:{}:{}",
          specifier,
          maybe_line_number.unwrap(),
          start_column
        ));
        #[allow(clippy::print_stderr)]
        {
          eprintln!("{}", msg);
        }
      }
    }
    ImportAssertionsSupport::CustomCallback(cb) => {
      cb(ImportAssertionsSupportCustomCallbackArgs {
        maybe_specifier: maybe_script_resource_name,
        maybe_line_number,
        column_number: start_column,
        maybe_source_line,
      });
    }
    _ => unreachable!(),
  }
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
  pub fn init_platform(
    v8_platform: Option<v8::SharedRef<v8::Platform>>,
    import_assertions_enabled: bool,
  ) {
    setup::init_v8(v8_platform, cfg!(test), false, import_assertions_enabled);
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
    import_assertions_enabled: bool,
  ) {
    setup::init_v8(
      v8_platform,
      cfg!(test),
      expose_natives,
      import_assertions_enabled,
    );
  }

  /// Only constructor, configuration is done through `options`.
  /// Panics if the runtime cannot be initialized.
  pub fn new(options: RuntimeOptions) -> JsRuntime {
    match Self::try_new(options) {
      Ok(runtime) => runtime,
      Err(err) => {
        panic!(
          "Failed to initialize a JsRuntime: {}",
          err.print_with_cause()
        );
      }
    }
  }

  /// Only constructor, configuration is done through `options`.
  /// Returns an error if the runtime cannot be initialized.
  pub fn try_new(mut options: RuntimeOptions) -> Result<JsRuntime, CoreError> {
    setup::init_v8(
      options.v8_platform.take(),
      cfg!(test),
      options.unsafe_expose_natives_and_gc(),
      options.import_assertions_support.is_enabled(),
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

  /// Returns the `OpMetadata` associated with the op `name`.
  /// Note this is linear with respect to the number of ops registered.
  pub fn op_metadata(&self, name: &str) -> Option<OpMetadata> {
    let state = &self.inner.main_realm.0.context_state;
    state.op_ctxs.iter().find_map(|ctx| {
      if ctx.decl.name == name {
        Some(ctx.decl.metadata)
      } else {
        None
      }
    })
  }

  fn new_inner(
    mut options: RuntimeOptions,
    will_snapshot: bool,
  ) -> Result<JsRuntime, CoreError> {
    let init_mode = InitMode::from_options(&options);
    let mut extensions = std::mem::take(&mut options.extensions);
    let mut isolate_allocations = IsolateAllocations::default();

    let enable_stack_trace_in_ops =
      options.maybe_op_stack_trace_callback.is_some();

    // First let's create an `OpState` and contribute to it from extensions...
    let mut op_state = OpState::new(options.maybe_op_stack_trace_callback);
    let unrefed_ops = op_state.unrefed_ops.clone();

    let lazy_extensions =
      extension_set::setup_op_state(&mut op_state, &mut extensions);

    // Load the sources and source maps
    let mut files_loaded = Vec::with_capacity(128);
    let loader = options
      .module_loader
      .unwrap_or_else(|| Rc::new(NoopModuleLoader));

    let mut source_mapper = SourceMapper::new(loader.clone());

    let (maybe_startup_snapshot, mut sidecar_data) = options
      .startup_snapshot
      .take()
      .map(snapshot::deconstruct)
      .unzip();

    let mut sources = extension_set::into_sources_and_source_maps(
      options.extension_transpiler.as_deref(),
      &extensions,
      sidecar_data.as_ref().map(|s| &*s.snapshot_data.extensions),
      |source| {
        mark_as_loaded_from_fs_during_snapshot(&mut files_loaded, &source.code)
      },
    )?;

    for loaded_source in sources
      .js
      .iter()
      .chain(sources.esm.iter())
      .chain(sources.lazy_esm.iter())
      .filter(|s| s.maybe_source_map.is_some())
    {
      source_mapper.add_ext_source_map(
        loaded_source.specifier.try_clone().unwrap(),
        loaded_source.maybe_source_map.clone().unwrap(),
      );
    }

    // ...now let's set up ` JsRuntimeState`, we'll need to set some fields
    // later, after `JsRuntime` is all set up...
    let waker = op_state.waker.clone();
    let op_state = Rc::new(RefCell::new(op_state));
    let (eval_context_get_code_cache_cb, eval_context_set_code_cache_cb) =
      options
        .eval_context_code_cache_cbs
        .map(|cbs| (Some(cbs.0), Some(cbs.1)))
        .unwrap_or_default();
    let state_rc = Rc::new(JsRuntimeState {
      source_mapper: RefCell::new(source_mapper),
      shared_array_buffer_store: options.shared_array_buffer_store,
      compiled_wasm_module_store: options.compiled_wasm_module_store,
      wait_for_inspector_disconnect_callback: options
        .wait_for_inspector_disconnect_callback,
      op_state: op_state.clone(),
      validate_import_attributes_cb: options.validate_import_attributes_cb,
      custom_module_evaluation_cb: options.custom_module_evaluation_cb,
      eval_context_get_code_cache_cb: RefCell::new(
        eval_context_get_code_cache_cb,
      ),
      eval_context_code_cache_ready_cb: RefCell::new(
        eval_context_set_code_cache_cb,
      ),
      waker,
      // Some fields are initialized later after isolate is created
      inspector: None.into(),
      has_inspector: false.into(),
      cppgc_template: None.into(),
      function_templates: Default::default(),
      callsite_prototype: None.into(),
      import_assertions_support: options.import_assertions_support,
      lazy_extensions,
    });

    // ...now we're moving on to ops; set them up, create `OpCtx` for each op
    // and get ready to actually create V8 isolate...
    let (op_decls, mut op_method_decls) =
      extension_set::init_ops(crate::ops_builtin::BUILTIN_OPS, &mut extensions);

    let op_driver = Rc::new(OpDriverImpl::default());
    let op_metrics_factory_fn = options.op_metrics_factory_fn.take();

    let (mut op_ctxs, methods_ctx_offset) = extension_set::create_op_ctxs(
      op_decls,
      &mut op_method_decls,
      op_metrics_factory_fn,
      op_driver.clone(),
      op_state.clone(),
      state_rc.clone(),
      enable_stack_trace_in_ops,
    );

    // ...ops are now almost fully set up; let's create a V8 isolate...
    let (
      global_template_middleware,
      global_object_middlewares,
      additional_references,
    ) = extension_set::get_middlewares_and_external_refs(&mut extensions);

    // Capture the extension, op and source counts
    let extensions = extensions.iter().map(|e| e.name).collect();
    let op_count = op_ctxs.len();
    let source_count = sources.len();
    let addl_refs_count = additional_references.len();

    let ops_in_snapshot = sidecar_data
      .as_ref()
      .map(|d| d.snapshot_data.op_count)
      .unwrap_or_default();
    let sources_in_snapshot = sidecar_data
      .as_ref()
      .map(|d| d.snapshot_data.source_count)
      .unwrap_or_default();

    let snapshot_sources: Vec<&[u8]> = sidecar_data
      .as_mut()
      .map(|s| std::mem::take(&mut s.snapshot_data.external_strings))
      .unwrap_or_default();
    (
      isolate_allocations.externalized_sources,
      isolate_allocations.original_sources,
    ) = bindings::externalize_sources(&mut sources, snapshot_sources);

    let external_references = bindings::create_external_references(
      &op_ctxs,
      &additional_references,
      &isolate_allocations.externalized_sources,
      ops_in_snapshot,
      sources_in_snapshot,
    );

    let has_snapshot = maybe_startup_snapshot.is_some();
    let mut isolate = setup::create_isolate(
      will_snapshot,
      options.create_params.take(),
      maybe_startup_snapshot,
      external_references.into(),
    );

    if state_rc.import_assertions_support.has_warning() {
      isolate.add_message_listener_with_error_level(
        isolate_message_listener,
        MessageErrorLevel::ALL,
      );
    }

    let isolate_ptr = isolate.as_mut() as *mut Isolate;
    // ...isolate is fully set up, we can forward its pointer to the ops to finish
    // their' setup...
    for op_ctx in op_ctxs.iter_mut() {
      op_ctx.isolate = isolate_ptr;
    }

    op_state.borrow_mut().put(isolate_ptr);

    // ...once ops and isolate are set up, we can create a `ContextState`...
    let context_state = Rc::new(ContextState::new(
      op_driver.clone(),
      isolate_ptr,
      op_ctxs,
      op_method_decls,
      methods_ctx_offset,
      op_state.borrow().external_ops_tracker.clone(),
      unrefed_ops,
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

    // ...and with `ContextState` available we can set up V8 context...
    let mut snapshotted_data = None;
    let main_context = {
      let scope = &mut v8::HandleScope::new(&mut isolate);

      let cppgc_template = crate::cppgc::make_cppgc_template(scope);
      state_rc
        .cppgc_template
        .borrow_mut()
        .replace(v8::Global::new(scope, cppgc_template));

      let context = create_context(
        scope,
        &global_template_middleware,
        &global_object_middlewares,
        has_snapshot,
      );

      // Get module map data from the snapshot
      if let Some(raw_data) = sidecar_data {
        snapshotted_data = Some(snapshot::load_snapshotted_data_from_snapshot(
          scope, context, raw_data,
        ));
      }

      v8::Global::new(scope, context)
    };

    let mut context_scope: v8::HandleScope =
      v8::HandleScope::with_context(&mut isolate, &main_context);
    let scope = &mut context_scope;
    let context = v8::Local::new(scope, &main_context);

    let callsite_prototype = crate::error::make_callsite_prototype(scope);
    state_rc
      .callsite_prototype
      .borrow_mut()
      .replace(v8::Global::new(scope, callsite_prototype));

    // ...followed by creation of `Deno.core` namespace, as well as internal
    // infrastructure to provide JavaScript bindings for ops...
    if init_mode == InitMode::New {
      bindings::initialize_deno_core_namespace(scope, context, init_mode);
      bindings::initialize_primordials_and_infra(scope)?;
    }
    // If we're creating a new runtime or there are new ops to register
    // set up JavaScript bindings for them.
    if init_mode.needs_ops_bindings() {
      bindings::initialize_deno_core_ops_bindings(
        scope,
        context,
        &context_state.op_ctxs,
        &context_state.op_method_decls,
        methods_ctx_offset,
        &mut state_rc.function_templates.borrow_mut(),
      );
    }

    // SAFETY: Initialize the context state slot.
    unsafe {
      context.set_aligned_pointer_in_embedder_data(
        super::jsrealm::CONTEXT_STATE_SLOT_INDEX,
        Rc::into_raw(context_state.clone()) as *mut c_void,
      );
    }

    let inspector = if options.inspector {
      Some(JsRuntimeInspector::new(
        isolate_ptr,
        scope,
        context,
        options.is_main,
      ))
    } else {
      None
    };

    // ...now that JavaScript bindings to ops are available we can deserialize
    // modules stored in the snapshot (because they depend on the ops and external
    // references must match properly) and recreate a module map...
    let exception_state = context_state.exception_state.clone();
    let module_map = Rc::new(ModuleMap::new(
      loader,
      exception_state.clone(),
      will_snapshot,
    ));

    if let Some((snapshotted_data, mut data_store)) = snapshotted_data {
      *exception_state.js_handled_promise_rejection_cb.borrow_mut() =
        snapshotted_data
          .js_handled_promise_rejection_cb
          .map(|cb| data_store.get(scope, cb));
      module_map.update_with_snapshotted_data(
        scope,
        &mut data_store,
        snapshotted_data.module_map_data,
      );

      if let Some(index) = snapshotted_data.ext_import_meta_proto {
        *context_state.ext_import_meta_proto.borrow_mut() =
          Some(data_store.get(scope, index));
      }

      state_rc
        .function_templates
        .borrow_mut()
        .update_with_snapshotted_data(
          scope,
          &mut data_store,
          snapshotted_data.function_templates_data,
        );

      let mut mapper = state_rc.source_mapper.borrow_mut();
      for (key, map) in snapshotted_data.ext_source_maps {
        mapper.add_ext_source_map(ModuleName::from_static(key), map.into());
      }
    }

    if context_state.ext_import_meta_proto.borrow().is_none() {
      let null = v8::null(scope);
      let obj =
        v8::Object::with_prototype_and_properties(scope, null.into(), &[], &[]);
      *context_state.ext_import_meta_proto.borrow_mut() =
        Some(v8::Global::new(scope, obj));
    }

    // SAFETY: Set the module map slot in the context
    unsafe {
      context.set_aligned_pointer_in_embedder_data(
        super::jsrealm::MODULE_MAP_SLOT_INDEX,
        Rc::into_raw(module_map.clone()) as *mut c_void,
      );
    }

    // ...we are ready to create a "realm" for the context...
    let main_realm = {
      let main_realm = JsRealmInner::new(
        context_state,
        main_context,
        module_map.clone(),
        state_rc.function_templates.clone(),
      );
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

    // ...which allows us to create the `JsRuntime` instance...
    let mut js_runtime = JsRuntime {
      inner: InnerIsolateState {
        will_snapshot,
        op_count,
        extensions,
        source_count,
        addl_refs_count,
        main_realm: ManuallyDrop::new(main_realm),
        state: ManuallyDropRc(ManuallyDrop::new(state_rc)),
        v8_isolate: ManuallyDrop::new(isolate),
      },
      allocations: isolate_allocations,
      files_loaded_from_fs_during_snapshot: vec![],
      is_main_runtime: options.is_main,
    };

    // ...we're almost done with the setup, all that's left is to execute
    // internal JS and then execute code provided by extensions...
    {
      let realm = JsRealm::clone(&js_runtime.inner.main_realm);
      let context_global = realm.context();
      let module_map = realm.0.module_map();

      // TODO(bartlomieju): this is somewhat duplicated in `bindings::initialize_context`,
      // but for migration period we need to have ops available in both `Deno.core.ops`
      // as well as have them available in "virtual ops module"
      // if !matches!(
      //   self.init_mode,
      //   InitMode::FromSnapshot {
      //     skip_op_registration: true
      //   }
      // ) {
      if init_mode == InitMode::New {
        js_runtime
          .execute_virtual_ops_module(context_global, module_map.clone());
      }

      if init_mode == InitMode::New {
        js_runtime.execute_builtin_sources(
          &realm,
          &module_map,
          &mut files_loaded,
        )?;
      }

      js_runtime.store_js_callbacks(&realm, will_snapshot);

      js_runtime.init_extension_js(
        &realm,
        &module_map,
        sources,
        options.extension_code_cache,
      )?;
    }

    if will_snapshot {
      js_runtime.files_loaded_from_fs_during_snapshot = files_loaded;
    }

    // ...and we've made it; `JsRuntime` is ready to execute user code.
    Ok(js_runtime)
  }

  /// If extensions were initialized with `lazy_init`, they need to be
  /// fully initialized with this method.
  pub fn lazy_init_extensions(
    &self,
    ext_args: Vec<ExtensionArguments>,
  ) -> Result<(), CoreError> {
    if ext_args.len() != self.inner.state.lazy_extensions.len() {
      return Err(
        CoreErrorKind::ExtensionLazyInitCountMismatch(
          ExtensionLazyInitCountMismatchError {
            lazy_init_extensions_len: self.inner.state.lazy_extensions.len(),
            arguments_len: ext_args.len(),
          },
        )
        .into_box(),
      );
    }

    let mut state = self.inner.state.op_state.borrow_mut();

    for (mut args, expected_name) in ext_args
      .into_iter()
      .zip(self.inner.state.lazy_extensions.iter())
    {
      if args.name != *expected_name {
        return Err(
          CoreErrorKind::ExtensionLazyInitOrderMismatch(
            ExtensionLazyInitOrderMismatchError {
              expected: expected_name,
              actual: args.name,
            },
          )
          .into_box(),
        );
      }

      let Some(f) = args.op_state_fn.take() else {
        continue;
      };

      f(&mut state);
    }

    Ok(())
  }

  pub fn set_eval_context_code_cache_cbs(
    &self,
    eval_context_code_cache_cbs: Option<(
      EvalContextGetCodeCacheCb,
      EvalContextCodeCacheReadyCb,
    )>,
  ) {
    let (eval_context_get_code_cache_cb, eval_context_set_code_cache_cb) =
      eval_context_code_cache_cbs
        .map(|cbs| (Some(cbs.0), Some(cbs.1)))
        .unwrap_or_default();
    *self.inner.state.eval_context_get_code_cache_cb.borrow_mut() =
      eval_context_get_code_cache_cb;
    *self
      .inner
      .state
      .eval_context_code_cache_ready_cb
      .borrow_mut() = eval_context_set_code_cache_cb;
  }

  #[cfg(test)]
  #[inline]
  pub(crate) fn module_map(&self) -> Rc<ModuleMap> {
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
  pub fn inspector(&self) -> Rc<RefCell<JsRuntimeInspector>> {
    self.inner.state.inspector()
  }

  #[inline]
  pub fn wait_for_inspector_disconnect(&self) {
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

  // TODO(bartlomieju): remove, instead `JsRuntimeForSnapshot::new` should return
  // a struct that contains this data.
  pub(crate) fn files_loaded_from_fs_during_snapshot(&self) -> &[&'static str] {
    &self.files_loaded_from_fs_during_snapshot
  }

  #[inline]
  pub fn handle_scope(&mut self) -> v8::HandleScope {
    let isolate = &mut self.inner.v8_isolate;
    self.inner.main_realm.handle_scope(isolate)
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
      &context_state.op_method_decls,
      context_state.methods_ctx_offset,
      scope,
      global,
    );
    let mod_id = module_map.new_synthetic_module(
      scope,
      VIRTUAL_OPS_MODULE_NAME,
      crate::ModuleType::JavaScript,
      synthetic_module_exports,
    );
    module_map.mod_evaluate_sync(scope, mod_id).unwrap();
  }

  /// Executes built-in scripts and ES modules - this code is required for
  /// ops system to work properly, as well as providing necessary bindings
  /// on the `Deno.core` namespace.
  ///
  /// This is not done in [`bindings::initialize_primordials_and_infra`] because
  /// some of this code already relies on certain ops being available.
  fn execute_builtin_sources(
    &mut self,
    realm: &JsRealm,
    module_map: &Rc<ModuleMap>,
    files_loaded: &mut Vec<&'static str>,
  ) -> Result<(), CoreError> {
    let scope = &mut realm.handle_scope(self.v8_isolate());

    for source_file in &BUILTIN_SOURCES {
      let name = source_file.specifier.v8_string(scope).unwrap();
      let source = source_file.source.v8_string(scope).unwrap();

      let origin = script_origin(scope, name, false, None);
      let script = v8::Script::compile(scope, source, Some(&origin))
        .ok_or(CoreModuleParseError(source_file.specifier))?;
      script
        .run(scope)
        .ok_or(CoreModuleExecuteError(source_file.specifier))?;
    }

    for file_source in &BUILTIN_ES_MODULES {
      mark_as_loaded_from_fs_during_snapshot(files_loaded, &file_source.code);
      module_map.lazy_load_es_module_with_code(
        scope,
        file_source.specifier,
        file_source.load()?,
        None,
      )?;
    }

    Ok(())
  }

  /// Initializes JS of provided Extensions in the given realm.
  async fn init_extension_js_inner(
    &mut self,
    realm: &JsRealm,
    module_map: &Rc<ModuleMap>,
    loaded_sources: LoadedSources,
    ext_code_cache: Option<Rc<dyn ExtCodeCache>>,
  ) -> Result<(), CoreError> {
    // First, add all the lazy ESM
    for source in loaded_sources.lazy_esm {
      module_map.add_lazy_loaded_esm_source(source.specifier, source.code);
    }

    // Temporarily override the loader of the `ModuleMap` so we can load
    // extension code.

    // TODO(bartlomieju): maybe this should be a method on the `ModuleMap`,
    // instead of explicitly changing the `.loader` field?
    let loader = module_map.loader.borrow().clone();
    let mut modules = Vec::with_capacity(loaded_sources.esm.len());
    let mut sources = Vec::with_capacity(loaded_sources.esm.len());
    for esm in loaded_sources.esm {
      modules.push(ModuleSpecifier::parse(&esm.specifier).unwrap());
      sources.push((esm.specifier, esm.code));
    }
    let ext_loader =
      Rc::new(ExtModuleLoader::new(sources, ext_code_cache.clone()));
    *module_map.loader.borrow_mut() = ext_loader.clone();

    // Next, load the extension modules as side modules (but do not execute them)
    for module in modules {
      // eprintln!("loading module: {module}");
      realm
        .load_side_es_module_from_code(self.v8_isolate(), &module, None)
        .await?;
    }

    // Execute extension scripts
    for source in loaded_sources.js {
      match &ext_code_cache {
        Some(ext_code_cache) => {
          let specifier = ModuleSpecifier::parse(&source.specifier)?;
          realm.execute_script_with_cache(
            self.v8_isolate(),
            specifier,
            source.code,
            &|specifier, code| {
              ext_code_cache.get_code_cache_info(specifier, code, false)
            },
            &|specifier, hash, code_cache| {
              ext_code_cache
                .code_cache_ready(specifier, hash, code_cache, false)
            },
          )?;
        }
        _ => {
          realm.execute_script(
            self.v8_isolate(),
            source.specifier,
            source.code,
          )?;
        }
      }
    }

    // ...then execute all entry points
    for specifier in loaded_sources.esm_entry_points {
      let Some(mod_id) =
        module_map.get_id(&specifier, RequestedModuleType::None)
      else {
        return Err(
          CoreErrorKind::MissingFromModuleMap(specifier.to_string()).into_box(),
        );
      };

      let isolate = self.v8_isolate();
      let scope = &mut realm.handle_scope(isolate);
      module_map.mod_evaluate_sync(scope, mod_id)?;
      let mut cx = Context::from_waker(Waker::noop());
      // poll once so code cache is populated. the `ExtCodeCache` trait is sync, so
      // the `CodeCacheReady` futures will always finish on the first poll.
      let _ = module_map.poll_progress(&mut cx, scope);
    }

    #[cfg(debug_assertions)]
    {
      let mut scope = realm.handle_scope(self.v8_isolate());
      module_map.check_all_modules_evaluated(&mut scope)?;
    }

    let module_map = realm.0.module_map();
    *module_map.loader.borrow_mut() = loader;
    ext_loader.finalize()?;

    Ok(())
  }

  /// Initializes JS of provided Extensions in the given realm.
  fn init_extension_js(
    &mut self,
    realm: &JsRealm,
    module_map: &Rc<ModuleMap>,
    loaded_sources: LoadedSources,
    ext_code_cache: Option<Rc<dyn ExtCodeCache>>,
  ) -> Result<(), CoreError> {
    futures::executor::block_on(self.init_extension_js_inner(
      realm,
      module_map,
      loaded_sources,
      ext_code_cache,
    ))?;

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

  /// Grab and store JavaScript bindings to callbacks necessary for the
  /// JsRuntime to operate properly.
  fn store_js_callbacks(&mut self, realm: &JsRealm, will_snapshot: bool) {
    let (event_loop_tick_cb, build_custom_error_cb, wasm_instance_fn) = {
      let scope = &mut realm.handle_scope(self.v8_isolate());
      let context = realm.context();
      let context_local = v8::Local::new(scope, context);
      let global = context_local.global(scope);
      // TODO(bartlomieju): these probably could be captured from main realm so we don't have to
      // look up them again?
      let deno_obj: v8::Local<v8::Object> =
        bindings::get(scope, global, DENO, "Deno");
      let core_obj: v8::Local<v8::Object> =
        bindings::get(scope, deno_obj, CORE, "Deno.core");

      let event_loop_tick_cb: v8::Local<v8::Function> = bindings::get(
        scope,
        core_obj,
        EVENT_LOOP_TICK,
        "Deno.core.eventLoopTick",
      );
      let build_custom_error_cb: v8::Local<v8::Function> = bindings::get(
        scope,
        core_obj,
        BUILD_CUSTOM_ERROR,
        "Deno.core.buildCustomError",
      );

      let mut wasm_instance_fn = None;
      if !will_snapshot {
        let key = WEBASSEMBLY.v8_string(scope).unwrap();
        if let Some(web_assembly_obj_value) = global.get(scope, key.into()) {
          // NOTE(bartlomieju): This is still fallible, because in some V8 modes
          // WebAssembly namespace is not available (eg. `--jitless`).
          let maybe_web_assembly_object =
            TryInto::<v8::Local<v8::Object>>::try_into(web_assembly_obj_value);
          if let Ok(web_assembly_object) = maybe_web_assembly_object {
            wasm_instance_fn = Some(bindings::get::<v8::Local<v8::Function>>(
              scope,
              web_assembly_object,
              INSTANCE,
              "WebAssembly.Instance",
            ));
          }
        }
      }

      (
        v8::Global::new(scope, event_loop_tick_cb),
        v8::Global::new(scope, build_custom_error_cb),
        wasm_instance_fn.map(|f| v8::Global::new(scope, f)),
      )
    };

    // Put global handles in the realm's ContextState
    let state_rc = realm.0.state();
    state_rc
      .js_event_loop_tick_cb
      .borrow_mut()
      .replace(event_loop_tick_cb);
    state_rc
      .exception_state
      .js_build_custom_error_cb
      .borrow_mut()
      .replace(build_custom_error_cb);
    if let Some(wasm_instance_fn) = wasm_instance_fn {
      state_rc
        .wasm_instance_fn
        .borrow_mut()
        .replace(wasm_instance_fn);
    }
  }

  /// Returns the runtime's op state, which can be used to maintain ops
  /// and access resources between op calls.
  pub fn op_state(&self) -> Rc<RefCell<OpState>> {
    self.inner.state.op_state.clone()
  }

  /// Returns the runtime's op names, ordered by OpId.
  pub fn op_names(&self) -> Vec<&'static str> {
    let state = &self.inner.main_realm.0.context_state;
    state.op_ctxs.iter().map(|o| o.decl.name).collect()
  }

  /// Executes traditional, non-ECMAScript-module JavaScript code, This code executes in
  /// the global scope by default, and it is possible to maintain local JS state and invoke
  /// this method multiple times.
  ///
  /// `name` may be any type that implements the internal [`IntoModuleName`] trait.
  /// It can be a filepath or any other string, but it is required to be 7-bit ASCII, eg.
  ///
  ///   - "/some/file/path.js"
  ///   - "<anon>"
  ///   - "[native code]"
  ///
  /// The same `name` value can be used for multiple executions.
  ///
  /// The source may be any type that implements the internal [`IntoModuleCodeString`] trait, but
  /// it is highly recommended that embedders use the [`ascii_str!`] to generate the fastest version
  /// of strings for v8 to handle. If the strings are not static, you may also pass a [`String`]
  /// generated by the [`format!`] macro.
  pub fn execute_script(
    &mut self,
    name: impl IntoModuleName,
    source_code: impl IntoModuleCodeString,
  ) -> Result<v8::Global<v8::Value>, JsError> {
    let isolate = &mut self.inner.v8_isolate;
    self.inner.main_realm.execute_script(
      isolate,
      name.into_module_name(),
      source_code.into_module_code(),
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
  ) -> impl Future<Output = Result<v8::Global<v8::Value>, JsError>> + use<> {
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
  ) -> impl Future<Output = Result<v8::Global<v8::Value>, JsError>> + use<> {
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
  ) -> impl Future<Output = Result<v8::Global<v8::Value>, JsError>> + use<> {
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
  ) -> impl Future<Output = Result<v8::Global<v8::Value>, JsError>> + use<> {
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
  ) -> Result<v8::Global<v8::Value>, CoreError> {
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
  ) -> Result<v8::Global<v8::Value>, CoreError> {
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
  ) -> Result<v8::Global<v8::Object>, CoreError> {
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
    &self,
    scope: &mut v8::HandleScope,
  ) -> Result<(), JsError> {
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
    let isolate_ptr = self.inner.v8_isolate.as_mut() as *mut _;
    let scope = &mut v8::HandleScope::with_context(
      self.inner.v8_isolate.as_mut(),
      context.clone(),
    );
    let context = v8::Local::new(scope, context);

    self.inner.state.has_inspector.set(true);
    **inspector = Some(JsRuntimeInspector::new(
      isolate_ptr,
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
  ) -> impl Future<Output = Result<v8::Global<v8::Value>, JsError>> + use<> {
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
  ) -> impl Future<Output = Result<v8::Global<v8::Value>, JsError>> + use<> {
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
  ) -> Result<v8::Global<v8::Value>, CoreError> {
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
  ///    `PollEventLoopOptions.wait_for_inspector` is set to true)
  pub async fn run_event_loop(
    &mut self,
    poll_options: PollEventLoopOptions,
  ) -> Result<(), CoreError> {
    poll_fn(|cx| self.poll_event_loop(cx, poll_options)).await
  }

  /// A utility function that run provided future concurrently with the event loop.
  ///
  /// If the event loop resolves while polling the future, it return an error with the text
  /// `Promise resolution is still pending but the event loop has already resolved`
  pub async fn with_event_loop_promise<'fut, T, E>(
    &mut self,
    mut fut: impl Future<Output = Result<T, E>> + Unpin + 'fut,
    poll_options: PollEventLoopOptions,
  ) -> Result<T, CoreError>
  where
    CoreError: From<E>,
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
        return Poll::Ready(Err(
          CoreErrorKind::PendingPromiseResolution.into_box(),
        ));
      }
      Poll::Pending
    })
    .await
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
  ) -> Result<T, E> {
    // Manually implement tokio::select
    poll_fn(|cx| {
      if let Poll::Ready(t) = fut.poll_unpin(cx) {
        return Poll::Ready(t);
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
  ) -> Poll<Result<(), CoreError>> {
    // SAFETY: We know this isolate is valid and non-null at this time
    let mut isolate_scope =
      v8::HandleScope::new(unsafe { &mut *self.v8_isolate_ptr() });
    let context =
      v8::Local::new(&mut isolate_scope, self.inner.main_realm.context());
    let mut scope = v8::ContextScope::new(&mut isolate_scope, context);
    self.poll_event_loop_inner(cx, &mut scope, poll_options)
  }

  fn poll_event_loop_inner(
    &self,
    cx: &mut Context,
    scope: &mut v8::HandleScope,
    poll_options: PollEventLoopOptions,
  ) -> Poll<Result<(), CoreError>> {
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
        let sessions_state = inspector.borrow().sessions_state();

        if poll_options.wait_for_inspector && sessions_state.has_active {
          if sessions_state.has_blocking {
            return Poll::Pending;
          }

          if sessions_state.has_nonblocking_wait_for_disconnect {
            let context = self.main_context();
            inspector.borrow_mut().context_destroyed(scope, context);
            self.wait_for_inspector_disconnect();
            return Poll::Pending;
          }
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
      if pending_state.has_pending_ops
        || pending_state.has_pending_dyn_imports
        || pending_state.has_pending_dyn_module_evaluation
        || pending_state.has_pending_background_tasks
        || pending_state.has_pending_external_ops
        || pending_state.has_tick_scheduled
      {
        // pass, will be polled again
      } else {
        return Poll::Ready(Err(
          CoreErrorKind::Js(find_and_report_stalled_level_await_in_any_realm(
            scope, &realm.0,
          ))
          .into_box(),
        ));
      }
    }

    if pending_state.has_pending_dyn_module_evaluation {
      if pending_state.has_pending_ops
        || pending_state.has_pending_dyn_imports
        || pending_state.has_pending_background_tasks
        || pending_state.has_pending_external_ops
        || pending_state.has_tick_scheduled
      {
        // pass, will be polled again
      } else if realm.modules_idle() {
        return Poll::Ready(Err(
          CoreErrorKind::Js(find_and_report_stalled_level_await_in_any_realm(
            scope, &realm.0,
          ))
          .into_box(),
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
) -> JsError {
  let module_map = inner_realm.module_map();
  let messages = module_map.find_stalled_top_level_await(scope);

  if !messages.is_empty() {
    // We are gonna print only a single message to provide a nice formatting
    // with source line of offending promise shown. Once user fixed it, then
    // they will get another error message for the next promise (but this
    // situation is gonna be very rare, if ever happening).
    let msg = v8::Local::new(scope, &messages[0]);
    let js_error = JsError::from_v8_message(scope, msg);
    return js_error;
  }

  unreachable!("Expected at least one stalled top-level await");
}

fn create_context<'a>(
  scope: &mut v8::HandleScope<'a, ()>,
  global_template_middlewares: &[GlobalTemplateMiddlewareFn],
  global_object_middlewares: &[GlobalObjectMiddlewareFn],
  has_snapshot: bool,
) -> v8::Local<'a, v8::Context> {
  let context = if has_snapshot {
    // Try to load the 1st index first, embedder may have used 0th for something else (like node:vm).
    v8::Context::from_snapshot(scope, 1, Default::default()).unwrap_or_else(
      || v8::Context::from_snapshot(scope, 0, Default::default()).unwrap(),
    )
  } else {
    // Set up the global object template and create context from it.
    let mut global_object_template = v8::ObjectTemplate::new(scope);
    for middleware in global_template_middlewares {
      global_object_template = middleware(scope, global_object_template);
    }

    global_object_template.set_internal_field_count(2);
    v8::Context::new(
      scope,
      v8::ContextOptions {
        global_template: Some(global_object_template),
        ..Default::default()
      },
    )
  };

  let scope = &mut v8::ContextScope::new(scope, context);

  let global = context.global(scope);
  for middleware in global_object_middlewares {
    middleware(scope, global);
  }
  context
}

impl JsRuntimeForSnapshot {
  /// Create a new runtime, panicking if the process fails.
  pub fn new(options: RuntimeOptions) -> JsRuntimeForSnapshot {
    match Self::try_new(options) {
      Ok(runtime) => runtime,
      Err(err) => {
        panic!("Failed to initialize JsRuntime for snapshotting: {:?}", err);
      }
    }
  }

  /// Try to create a new runtime, returning an error if the process fails.
  pub fn try_new(
    mut options: RuntimeOptions,
  ) -> Result<JsRuntimeForSnapshot, CoreError> {
    setup::init_v8(
      options.v8_platform.take(),
      true,
      options.unsafe_expose_natives_and_gc(),
      options.import_assertions_support.is_enabled(),
    );

    let runtime = JsRuntime::new_inner(options, true)?;
    Ok(JsRuntimeForSnapshot(runtime))
  }

  /// Takes a snapshot and consumes the runtime.
  ///
  /// `Error` can usually be downcast to `JsError`.
  pub fn snapshot(mut self) -> Box<[u8]> {
    // Ensure there are no live inspectors to prevent crashes.
    self.inner.prepare_for_cleanup();
    let original_sources =
      std::mem::take(&mut self.0.allocations.original_sources);
    let external_strings = original_sources
      .iter()
      .map(|s| s.as_str().as_bytes())
      .collect();
    let realm = JsRealm::clone(&self.inner.main_realm);

    // Set the context to be snapshot's default context
    {
      let mut scope = realm.handle_scope(self.v8_isolate());
      let default_context = v8::Context::new(&mut scope, Default::default());
      scope.set_default_context(default_context);

      let local_context = v8::Local::new(&mut scope, realm.context());
      scope.add_context(local_context);
    }

    // Borrow the source maps during the snapshot to avoid copies
    let source_maps = self
      .inner
      .state
      .source_mapper
      .borrow_mut()
      .take_ext_source_maps();
    let mut ext_source_maps = HashMap::with_capacity(source_maps.len());
    for (k, v) in &source_maps {
      ext_source_maps.insert(k.as_static_str().unwrap(), v.as_ref());
    }

    // Serialize the module map and store its data in the snapshot.
    // TODO(mmastrac): This should deconstruct the realm into sidecar data rather than
    // extracting it from the realm and then tearing the realm down. IE, this should
    // probably be a method on `JsRealm` with a self-consuming parameter signature:
    // `fn into_sidecar_data(self) -> ...`.
    let sidecar_data = {
      let mut data_store = SnapshotStoreDataStore::default();
      let module_map_data = {
        let module_map = realm.0.module_map();
        module_map.serialize_for_snapshotting(&mut data_store)
      };
      let function_templates_data = {
        let function_templates = realm.0.function_templates();
        let f = std::mem::take(&mut *function_templates.borrow_mut());

        f.serialize_for_snapshotting(&mut data_store)
      };
      let maybe_js_handled_promise_rejection_cb = {
        let context_state = &realm.0.context_state;
        let exception_state = &context_state.exception_state;
        exception_state
          .js_handled_promise_rejection_cb
          .borrow()
          .clone()
      }
      .map(|cb| data_store.register(cb));

      let ext_import_meta_proto = realm
        .0
        .context_state
        .ext_import_meta_proto
        .borrow()
        .clone()
        .map(|p| data_store.register(p));

      let snapshotted_data = SnapshottedData {
        module_map_data,
        function_templates_data,
        op_count: self.inner.op_count,
        addl_refs_count: self.inner.addl_refs_count,
        source_count: self.inner.source_count,
        extensions: self.inner.extensions.clone(),
        js_handled_promise_rejection_cb: maybe_js_handled_promise_rejection_cb,
        ext_import_meta_proto,
        ext_source_maps,
        external_strings,
      };

      let mut scope = realm.handle_scope(self.v8_isolate());
      snapshot::store_snapshotted_data_for_snapshot(
        &mut scope,
        realm.context().clone(),
        snapshotted_data,
        data_store,
      )
    };
    drop(realm);

    let v8_data = self
      .0
      .inner
      .prepare_for_snapshot()
      .create_blob(v8::FunctionCodeHandling::Keep)
      .unwrap();

    snapshot::serialize(v8_data, sidecar_data)
  }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct EventLoopPendingState {
  has_pending_ops: bool,
  has_pending_refed_ops: bool,
  has_pending_dyn_imports: bool,
  has_pending_dyn_module_evaluation: bool,
  has_pending_module_evaluation: bool,
  has_pending_background_tasks: bool,
  has_tick_scheduled: bool,
  has_pending_promise_events: bool,
  has_pending_external_ops: bool,
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
    let has_pending_timers = !state.timers.is_empty();
    let has_pending_refed_timers = state.timers.has_pending_timers();
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
    let has_pending_refed_ops = has_pending_tasks
      || has_pending_refed_timers
      || num_pending_ops > num_unrefed_ops;
    EventLoopPendingState {
      has_pending_ops: has_pending_refed_ops
        || has_pending_timers
        || (num_pending_ops > 0),
      has_pending_refed_ops,
      has_pending_dyn_imports,
      has_pending_dyn_module_evaluation,
      has_pending_module_evaluation,
      has_pending_background_tasks: scope.has_pending_background_tasks(),
      has_tick_scheduled: state.has_next_tick_scheduled.get(),
      has_pending_promise_events,
      has_pending_external_ops: state.external_ops_tracker.has_pending_ops(),
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
      || self.has_pending_external_ops
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
  ) -> impl Future<Output = Result<(), CoreError>> + use<> {
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
  /// The source may be any type that implements the internal [`IntoModuleCodeString`] trait, but
  /// it is highly recommended that embedders use the [`ascii_str!`] to generate the fastest version
  /// of strings for v8 to handle. If the strings are not static, you may also pass a [`String`]
  /// generated by the [`format!`] macro.
  ///
  /// User must call [`JsRuntime::mod_evaluate`] with returned `ModuleId`
  /// manually after load is finished.
  pub async fn load_main_es_module_from_code(
    &mut self,
    specifier: &ModuleSpecifier,
    code: impl IntoModuleCodeString,
  ) -> Result<ModuleId, CoreError> {
    let isolate = &mut self.inner.v8_isolate;
    self
      .inner
      .main_realm
      .load_main_es_module_from_code(
        isolate,
        specifier,
        Some(code.into_module_code()),
      )
      .await
  }

  /// Asynchronously load specified module and all of its dependencies, retrieving
  /// the module from the supplied [`ModuleLoader`].
  ///
  /// The module will be marked as "main", and because of that
  /// "import.meta.main" will return true when checked inside that module.
  ///
  /// User must call [`JsRuntime::mod_evaluate`] with returned `ModuleId`
  /// manually after load is finished.
  pub async fn load_main_es_module(
    &mut self,
    specifier: &ModuleSpecifier,
  ) -> Result<ModuleId, CoreError> {
    let isolate = &mut self.inner.v8_isolate;
    self
      .inner
      .main_realm
      .load_main_es_module_from_code(isolate, specifier, None)
      .await
  }

  /// Asynchronously load specified ES module and all of its dependencies from the
  /// provided source.
  ///
  /// This method is meant to be used when loading some utility code that
  /// might be later imported by the main module (ie. an entry point module).
  ///
  /// The source may be any type that implements the internal [`IntoModuleCodeString`] trait, but
  /// it is highly recommended that embedders use the [`ascii_str!`] to generate the fastest version
  /// of strings for v8 to handle. If the strings are not static, you may also pass a [`String`]
  /// generated by the [`format!`] macro.
  ///
  /// User must call [`JsRuntime::mod_evaluate`] with returned `ModuleId`
  /// manually after load is finished.
  pub async fn load_side_es_module_from_code(
    &mut self,
    specifier: &ModuleSpecifier,
    code: impl IntoModuleCodeString,
  ) -> Result<ModuleId, CoreError> {
    let isolate = &mut self.inner.v8_isolate;
    self
      .inner
      .main_realm
      .load_side_es_module_from_code(
        isolate,
        specifier,
        Some(code.into_module_code()),
      )
      .await
  }

  /// Asynchronously load specified ES module and all of its dependencies, retrieving
  /// the module from the supplied [`ModuleLoader`].
  ///
  /// This method is meant to be used when loading some utility code that
  /// might be later imported by the main module (ie. an entry point module).
  ///
  /// User must call [`JsRuntime::mod_evaluate`] with returned `ModuleId`
  /// manually after load is finished.
  pub async fn load_side_es_module(
    &mut self,
    specifier: &ModuleSpecifier,
  ) -> Result<ModuleId, CoreError> {
    let isolate = &mut self.inner.v8_isolate;
    self
      .inner
      .main_realm
      .load_side_es_module_from_code(isolate, specifier, None)
      .await
  }

  /// Load and evaluate an ES module provided the specifier and source code.
  ///
  /// The module should not have Top-Level Await (that is, it should be
  /// possible to evaluate it synchronously).
  ///
  /// It is caller's responsibility to ensure that not duplicate specifiers are
  /// passed to this method.
  pub fn lazy_load_es_module_with_code(
    &mut self,
    specifier: impl IntoModuleName,
    code: impl IntoModuleCodeString,
  ) -> Result<v8::Global<v8::Value>, CoreError> {
    let isolate = &mut self.inner.v8_isolate;
    self.inner.main_realm.lazy_load_es_module_with_code(
      isolate,
      specifier.into_module_name(),
      code.into_module_code(),
    )
  }

  fn do_js_event_loop_tick_realm(
    cx: &mut Context,
    scope: &mut v8::HandleScope,
    context_state: &ContextState,
    exception_state: &ExceptionState,
  ) -> Result<bool, JsError> {
    let mut dispatched_ops = false;

    // Poll any pending task spawner tasks. Note that we need to poll separately because otherwise
    // Rust will extend the lifetime of the borrow longer than we expect.
    let mut retries = 3;
    while let Poll::Ready(tasks) =
      context_state.task_spawner_factory.poll_inner(cx)
    {
      // TODO(mmastrac): we are using this flag
      dispatched_ops = true;
      for task in tasks {
        task(scope);
      }
      // We may need to perform a microtask checkpoint here
      scope.perform_microtask_checkpoint();

      // We don't want tasks that spawn other tasks to starve the event loop, so break
      // after three times around and allow the remainder of the event loop to spin.
      retries -= 1;
      if retries == 0 {
        cx.waker().wake_by_ref();
        break;
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

      let res = res.unwrap(scope);

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
      context_state
        .activity_traces
        .complete(RuntimeActivityType::AsyncOp, promise_id as _);
      dispatched_ops |= true;
      args.push(v8::Integer::new(scope, promise_id).into());
      args.push(v8::Boolean::new(scope, res.is_ok()).into());
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
      // Avoid holding the pending rejection lock longer than necessary
      let mut pending_rejections =
        exception_state.pending_promise_rejections.borrow_mut();
      let mut rejections = VecDeque::default();
      std::mem::swap(&mut *pending_rejections, &mut rejections);
      drop(pending_rejections);

      let arr = v8::Array::new(scope, (rejections.len() * 2) as i32);
      let mut index = 0;
      for rejection in rejections.into_iter() {
        let value = v8::Local::new(scope, rejection.0);
        arr.set_index(scope, index, value.into());
        index += 1;
        let value = v8::Local::new(scope, rejection.1);
        arr.set_index(scope, index, value);
        index += 1;
      }
      arr.into()
    } else {
      undefined
    };

    args.push(rejections);

    // TODO(mmastrac): timer dispatch should be done via direct function call, but we will have to start
    // storing the exception-reporting callback.
    let timers = match context_state.timers.poll_timers(cx) {
      Poll::Ready(timers) => {
        let traces_enabled = context_state.activity_traces.is_enabled();
        let arr = v8::Array::new(scope, (timers.len() * 3) as _);
        #[allow(clippy::needless_range_loop)]
        for i in 0..timers.len() {
          if traces_enabled {
            // Timer and interval traces both use RuntimeActivityType::Timer
            context_state
              .activity_traces
              .complete(RuntimeActivityType::Timer, timers[i].0 as _);
          }
          // depth, id, function
          let value = v8::Integer::new(scope, timers[i].1.1 as _);
          arr.set_index(scope, (i * 3) as _, value.into());
          let value = v8::Number::new(scope, timers[i].0 as _);
          arr.set_index(scope, (i * 3 + 1) as _, value.into());
          let value = v8::Local::new(scope, timers[i].1.0.clone());
          arr.set_index(scope, (i * 3 + 2) as _, value.into());
        }
        arr.into()
      }
      _ => undefined,
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

fn mark_as_loaded_from_fs_during_snapshot(
  files_loaded: &mut Vec<&'static str>,
  source: &ExtensionFileSourceCode,
) {
  #[allow(deprecated)]
  if let ExtensionFileSourceCode::LoadedFromFsDuringSnapshot(path) = source {
    files_loaded.push(path);
  }
}
