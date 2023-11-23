// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use crate::error::generic_error;
use crate::fast_string::FastString;
use crate::module_specifier::ModuleSpecifier;
use crate::resolve_url;
use anyhow::Error;
use futures::future::FutureExt;
use futures::stream::FuturesUnordered;
use futures::stream::Stream;
use futures::stream::TryStreamExt;
use serde::Deserialize;
use serde::Serialize;
use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::Context;
use std::task::Poll;

mod loaders;
mod map;
mod module_map_data;

#[cfg(test)]
mod tests;

#[cfg(test)]
pub use loaders::CountingModuleLoader;
#[cfg(test)]
pub use loaders::LoggingModuleLoader;
#[cfg(test)]
pub use loaders::ModuleLoadEventCounts;

pub(crate) use loaders::ExtModuleLoader;
pub use loaders::ExtModuleLoaderCb;
pub use loaders::FsModuleLoader;
pub use loaders::ModuleLoader;
pub use loaders::NoopModuleLoader;
pub use loaders::StaticModuleLoader;
pub(crate) use map::ModuleMap;

pub type ModuleId = usize;
pub(crate) type ModuleLoadId = i32;
pub type ModuleCode = FastString;
pub type ModuleName = FastString;

/// Callback to validate import attributes. If the validation fails and exception
/// should be thrown using `scope.throw_exception()`.
pub type ValidateImportAttributesCb =
  Box<dyn Fn(&mut v8::HandleScope, &HashMap<String, String>)>;

const SUPPORTED_TYPE_ASSERTIONS: &[&str] = &["json"];

/// Throws a `TypeError` if `type` attribute is not equal to "json". Allows
/// all other attributes.
pub(crate) fn validate_import_attributes(
  scope: &mut v8::HandleScope,
  assertions: &HashMap<String, String>,
) {
  for (key, value) in assertions {
    let msg = if key != "type" {
      Some(format!("\"{key}\" attribute is not supported."))
    } else if !SUPPORTED_TYPE_ASSERTIONS.contains(&value.as_str()) {
      Some(format!("\"{value}\" is not a valid module type."))
    } else {
      None
    };

    let Some(msg) = msg else {
      continue;
    };

    let message = v8::String::new(scope, &msg).unwrap();
    let exception = v8::Exception::type_error(scope, message);
    scope.throw_exception(exception);
    return;
  }
}

#[derive(Debug)]
pub(crate) enum ImportAssertionsKind {
  StaticImport,
  DynamicImport,
}

pub(crate) fn parse_import_assertions(
  scope: &mut v8::HandleScope,
  import_assertions: v8::Local<v8::FixedArray>,
  kind: ImportAssertionsKind,
) -> HashMap<String, String> {
  let mut assertions: HashMap<String, String> = HashMap::default();

  let assertions_per_line = match kind {
    // For static imports, assertions are triples of (keyword, value and source offset)
    // Also used in `module_resolve_callback`.
    ImportAssertionsKind::StaticImport => 3,
    // For dynamic imports, assertions are tuples of (keyword, value)
    ImportAssertionsKind::DynamicImport => 2,
  };
  assert_eq!(import_assertions.length() % assertions_per_line, 0);
  let no_of_assertions = import_assertions.length() / assertions_per_line;

  for i in 0..no_of_assertions {
    let assert_key = import_assertions
      .get(scope, assertions_per_line * i)
      .unwrap();
    let assert_key_val = v8::Local::<v8::Value>::try_from(assert_key).unwrap();
    let assert_value = import_assertions
      .get(scope, (assertions_per_line * i) + 1)
      .unwrap();
    let assert_value_val =
      v8::Local::<v8::Value>::try_from(assert_value).unwrap();
    assertions.insert(
      assert_key_val.to_rust_string_lossy(scope),
      assert_value_val.to_rust_string_lossy(scope),
    );
  }

  assertions
}

pub(crate) fn get_asserted_module_type_from_assertions(
  assertions: &HashMap<String, String>,
) -> AssertedModuleType {
  assertions
    .get("type")
    .map(|ty| {
      if ty == "json" {
        AssertedModuleType::Json
      } else {
        AssertedModuleType::JavaScriptOrWasm
      }
    })
    .unwrap_or(AssertedModuleType::JavaScriptOrWasm)
}

/// A type of module to be executed.
///
/// For non-`JavaScript` modules, this value doesn't tell
/// how to interpret the module; it is only used to validate
/// the module against an import assertion (if one is present
/// in the import statement).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[repr(u32)]
pub enum ModuleType {
  JavaScript,
  Json,
}

impl std::fmt::Display for ModuleType {
  fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
    match self {
      Self::JavaScript => write!(f, "JavaScript"),
      Self::Json => write!(f, "JSON"),
    }
  }
}

/// EsModule source code that will be loaded into V8.
///
/// Users can implement `Into<ModuleInfo>` for different file types that
/// can be transpiled to valid EsModule.
///
/// Found module URL might be different from specified URL
/// used for loading due to redirections (like HTTP 303).
/// Eg. Both "`https://example.com/a.ts`" and
/// "`https://example.com/b.ts`" may point to "`https://example.com/c.ts`"
/// By keeping track of specified and found URL we can alias modules and avoid
/// recompiling the same code 3 times.
// TODO(bartlomieju): I have a strong opinion we should store all redirects
// that happened; not only first and final target. It would simplify a lot
// of things throughout the codebase otherwise we may end up requesting
// intermediate redirects from file loader.
// NOTE: This should _not_ be made #[derive(Clone)] unless we take some precautions to avoid excessive string copying.
#[derive(Debug)]
pub struct ModuleSource {
  pub code: ModuleCode,
  pub module_type: ModuleType,
  module_url_specified: ModuleName,
  /// If the module was found somewhere other than the specified address, this will be [`Some`].
  module_url_found: Option<ModuleName>,
}

impl ModuleSource {
  /// Create a [`ModuleSource`] without a redirect.
  pub fn new(
    module_type: impl Into<ModuleType>,
    code: ModuleCode,
    specifier: &ModuleSpecifier,
  ) -> Self {
    let module_url_specified = specifier.as_ref().to_owned().into();
    Self {
      code,
      module_type: module_type.into(),
      module_url_specified,
      module_url_found: None,
    }
  }

  /// Create a [`ModuleSource`] with a potential redirect. If the `specifier_found` parameter is the same as the
  /// specifier, the code behaves the same was as `ModuleSource::new`.
  pub fn new_with_redirect(
    module_type: impl Into<ModuleType>,
    code: ModuleCode,
    specifier: &ModuleSpecifier,
    specifier_found: &ModuleSpecifier,
  ) -> Self {
    let module_url_found = if specifier == specifier_found {
      None
    } else {
      Some(specifier_found.as_ref().to_owned().into())
    };
    let module_url_specified = specifier.as_ref().to_owned().into();
    Self {
      code,
      module_type: module_type.into(),
      module_url_specified,
      module_url_found,
    }
  }

  #[cfg(test)]
  pub fn for_test(code: &'static str, file: impl AsRef<str>) -> Self {
    Self {
      code: ModuleCode::from_static(code),
      module_type: ModuleType::JavaScript,
      module_url_specified: file.as_ref().to_owned().into(),
      module_url_found: None,
    }
  }

  /// If the `found` parameter is the same as the `specified` parameter, the code behaves the same was as `ModuleSource::for_test`.
  #[cfg(test)]
  pub fn for_test_with_redirect(
    code: &'static str,
    specified: impl AsRef<str>,
    found: impl AsRef<str>,
  ) -> Self {
    let specified = specified.as_ref().to_string();
    let found = found.as_ref().to_string();
    let found = if found == specified {
      None
    } else {
      Some(found.into())
    };
    Self {
      code: ModuleCode::from_static(code),
      module_type: ModuleType::JavaScript,
      module_url_specified: specified.into(),
      module_url_found: found,
    }
  }
}

pub(crate) type PrepareLoadFuture =
  dyn Future<Output = (ModuleLoadId, Result<RecursiveModuleLoad, Error>)>;
pub type ModuleSourceFuture = dyn Future<Output = Result<ModuleSource, Error>>;

type ModuleLoadFuture =
  dyn Future<Output = Result<Option<(ModuleRequest, ModuleSource)>, Error>>;

#[derive(Debug, PartialEq, Eq)]
pub enum ResolutionKind {
  /// This kind is used in only one situation: when a module is loaded via
  /// `JsRuntime::load_main_module` and is the top-level module, ie. the one
  /// passed as an argument to `JsRuntime::load_main_module`.
  MainModule,
  /// This kind is returned for all other modules during module load, that are
  /// static imports.
  Import,
  /// This kind is returned for all modules that are loaded as a result of a
  /// call to `import()` API (ie. top-level module as well as all its
  /// dependencies, and any other `import()` calls from that load).
  DynamicImport,
}

/// Describes the entrypoint of a recursive module load.
#[derive(Debug)]
enum LoadInit {
  /// Main module specifier.
  Main(String),
  /// Module specifier for side module.
  Side(String),
  /// Dynamic import specifier with referrer and expected
  /// module type (which is determined by import assertion).
  DynamicImport(String, String, AssertedModuleType),
}

#[derive(Debug, Eq, PartialEq)]
pub enum LoadState {
  Init,
  LoadingRoot,
  LoadingImports,
  Done,
}

/// This future is used to implement parallel async module loading.
pub(crate) struct RecursiveModuleLoad {
  pub id: ModuleLoadId,
  pub root_module_id: Option<ModuleId>,
  init: LoadInit,
  root_asserted_module_type: Option<AssertedModuleType>,
  state: LoadState,
  module_map_rc: Rc<ModuleMap>,
  pending: FuturesUnordered<Pin<Box<ModuleLoadFuture>>>,
  visited: HashSet<ModuleRequest>,
  visited_as_alias: Rc<RefCell<HashSet<String>>>,
  // The loader is copied from `module_map_rc`, but its reference is cloned
  // ahead of time to avoid already-borrowed errors.
  loader: Rc<dyn ModuleLoader>,
}

impl RecursiveModuleLoad {
  /// Starts a new asynchronous load of the module graph for given specifier.
  ///
  /// The module corresponding for the given `specifier` will be marked as
  // "the main module" (`import.meta.main` will return `true` for this module).
  fn main(specifier: &str, module_map_rc: Rc<ModuleMap>) -> Self {
    Self::new(LoadInit::Main(specifier.to_string()), module_map_rc)
  }

  /// Starts a new asynchronous load of the module graph for given specifier.
  fn side(specifier: &str, module_map_rc: Rc<ModuleMap>) -> Self {
    Self::new(LoadInit::Side(specifier.to_string()), module_map_rc)
  }

  /// Starts a new asynchronous load of the module graph for given specifier
  /// that was imported using `import()`.
  fn dynamic_import(
    specifier: &str,
    referrer: &str,
    asserted_module_type: AssertedModuleType,
    module_map_rc: Rc<ModuleMap>,
  ) -> Self {
    Self::new(
      LoadInit::DynamicImport(
        specifier.to_string(),
        referrer.to_string(),
        asserted_module_type,
      ),
      module_map_rc,
    )
  }

  fn new(init: LoadInit, module_map_rc: Rc<ModuleMap>) -> Self {
    let id = module_map_rc.next_load_id();
    let loader = module_map_rc.loader.borrow().clone();
    let asserted_module_type = match &init {
      LoadInit::DynamicImport(_, _, module_type) => module_type.clone(),
      _ => AssertedModuleType::JavaScriptOrWasm,
    };
    let mut load = Self {
      id,
      root_module_id: None,
      root_asserted_module_type: None,
      init,
      state: LoadState::Init,
      module_map_rc: module_map_rc.clone(),
      loader,
      pending: FuturesUnordered::new(),
      visited: HashSet::new(),
      visited_as_alias: Default::default(),
    };
    // FIXME(bartlomieju): this seems fishy
    // Ignore the error here, let it be hit in `Stream::poll_next()`.
    if let Ok(root_specifier) = load.resolve_root() {
      if let Some(module_id) =
        module_map_rc.get_id(root_specifier, &asserted_module_type)
      {
        load.root_module_id = Some(module_id);
        load.root_asserted_module_type = Some(asserted_module_type);
      }
    }
    load
  }

  fn resolve_root(&self) -> Result<ModuleSpecifier, Error> {
    match self.init {
      LoadInit::Main(ref specifier) => {
        self
          .loader
          .resolve(specifier, ".", ResolutionKind::MainModule)
      }
      LoadInit::Side(ref specifier) => {
        self.loader.resolve(specifier, ".", ResolutionKind::Import)
      }
      LoadInit::DynamicImport(ref specifier, ref referrer, _) => self
        .loader
        .resolve(specifier, referrer, ResolutionKind::DynamicImport),
    }
  }

  async fn prepare(&self) -> Result<(), Error> {
    let (module_specifier, maybe_referrer) = match self.init {
      LoadInit::Main(ref specifier) => {
        let spec =
          self
            .loader
            .resolve(specifier, ".", ResolutionKind::MainModule)?;
        (spec, None)
      }
      LoadInit::Side(ref specifier) => {
        let spec =
          self
            .loader
            .resolve(specifier, ".", ResolutionKind::Import)?;
        (spec, None)
      }
      LoadInit::DynamicImport(ref specifier, ref referrer, _) => {
        let spec = self.loader.resolve(
          specifier,
          referrer,
          ResolutionKind::DynamicImport,
        )?;
        (spec, Some(referrer.to_string()))
      }
    };

    self
      .loader
      .prepare_load(&module_specifier, maybe_referrer, self.is_dynamic_import())
      .await
  }

  fn is_currently_loading_main_module(&self) -> bool {
    !self.is_dynamic_import()
      && matches!(self.init, LoadInit::Main(..))
      && self.state == LoadState::LoadingRoot
  }

  fn is_dynamic_import(&self) -> bool {
    matches!(self.init, LoadInit::DynamicImport(..))
  }

  pub(crate) fn register_and_recurse(
    &mut self,
    scope: &mut v8::HandleScope,
    module_request: &ModuleRequest,
    module_source: ModuleSource,
  ) -> Result<(), ModuleError> {
    let asserted_module_type = module_request.asserted_module_type.clone();
    if asserted_module_type != module_source.module_type {
      return Err(ModuleError::Other(generic_error(format!(
        "Expected a \"{}\" module but loaded a \"{}\" module.",
        asserted_module_type, module_source.module_type,
      ))));
    }

    let module_id = self.module_map_rc.new_module(
      scope,
      self.is_currently_loading_main_module(),
      self.is_dynamic_import(),
      module_source,
    )?;

    self.register_and_recurse_inner(module_id, module_request);

    // Update `self.state` however applicable.
    if self.state == LoadState::LoadingRoot {
      self.root_module_id = Some(module_id);
      self.root_asserted_module_type = Some(asserted_module_type);
      self.state = LoadState::LoadingImports;
    }
    if self.pending.is_empty() {
      self.state = LoadState::Done;
    }

    Ok(())
  }

  fn register_and_recurse_inner(
    &mut self,
    module_id: usize,
    module_request: &ModuleRequest,
  ) {
    // Recurse the module's imports. There are two cases for each import:
    // 1. If the module is not in the module map, start a new load for it in
    //    `self.pending`. The result of that load should eventually be passed to
    //    this function for recursion.
    // 2. If the module is already in the module map, queue it up to be
    //    recursed synchronously here.
    // This robustly ensures that the whole graph is in the module map before
    // `LoadState::Done` is set.
    let mut already_registered = VecDeque::new();
    already_registered.push_back((module_id, module_request.clone()));
    self.visited.insert(module_request.clone());
    while let Some((module_id, module_request)) = already_registered.pop_front()
    {
      let referrer = ModuleSpecifier::parse(&module_request.specifier).unwrap();
      let imports = self
        .module_map_rc
        .get_requested_modules(module_id)
        .unwrap()
        .clone();
      for module_request in imports {
        if !self.visited.contains(&module_request)
          && !self
            .visited_as_alias
            .borrow()
            .contains(&module_request.specifier)
        {
          if let Some(module_id) = self.module_map_rc.get_id(
            module_request.specifier.as_str(),
            &module_request.asserted_module_type,
          ) {
            already_registered.push_back((module_id, module_request.clone()));
          } else {
            let request = module_request.clone();
            let specifier =
              ModuleSpecifier::parse(&module_request.specifier).unwrap();
            let visited_as_alias = self.visited_as_alias.clone();
            let referrer = referrer.clone();
            let loader = self.loader.clone();
            let is_dynamic_import = self.is_dynamic_import();
            let fut = async move {
              // `visited_as_alias` unlike `visited` is checked as late as
              // possible because it can only be populated after completed
              // loads, meaning a duplicate load future may have already been
              // dispatched before we know it's a duplicate.
              if visited_as_alias.borrow().contains(specifier.as_str()) {
                return Ok(None);
              }
              let load_result = loader
                .load(&specifier, Some(&referrer), is_dynamic_import)
                .await;
              if let Ok(source) = &load_result {
                if let Some(found_specifier) = &source.module_url_found {
                  visited_as_alias
                    .borrow_mut()
                    .insert(found_specifier.as_str().to_string());
                }
              }
              load_result.map(|s| Some((request, s)))
            };
            self.pending.push(fut.boxed_local());
          }
          self.visited.insert(module_request);
        }
      }
    }
  }
}

impl Stream for RecursiveModuleLoad {
  type Item = Result<(ModuleRequest, ModuleSource), Error>;

  fn poll_next(
    self: Pin<&mut Self>,
    cx: &mut Context,
  ) -> Poll<Option<Self::Item>> {
    let inner = self.get_mut();
    // IMPORTANT: Do not borrow `inner.module_map_rc` here. It may not be
    // available.
    match inner.state {
      LoadState::Init => {
        let module_specifier = match inner.resolve_root() {
          Ok(url) => url,
          Err(error) => return Poll::Ready(Some(Err(error))),
        };
        let asserted_module_type = match &inner.init {
          LoadInit::DynamicImport(_, _, module_type) => module_type.clone(),
          _ => AssertedModuleType::JavaScriptOrWasm,
        };
        let module_request = ModuleRequest {
          specifier: module_specifier.to_string(),
          asserted_module_type,
        };
        let load_fut = if let Some(module_id) = inner.root_module_id {
          // If the inner future is already in the map, we might be done (assuming there are no pending
          // loads).
          inner.register_and_recurse_inner(module_id, &module_request);
          if inner.pending.is_empty() {
            inner.state = LoadState::Done;
          } else {
            inner.state = LoadState::LoadingImports;
          }
          // Internally re-poll using the new state to avoid spinning the event loop again.
          return Self::poll_next(Pin::new(inner), cx);
        } else {
          let maybe_referrer = match inner.init {
            LoadInit::DynamicImport(_, ref referrer, _) => {
              resolve_url(referrer).ok()
            }
            _ => None,
          };
          let loader = inner.loader.clone();
          let is_dynamic_import = inner.is_dynamic_import();
          async move {
            let result = loader
              .load(
                &module_specifier,
                maybe_referrer.as_ref(),
                is_dynamic_import,
              )
              .await;
            result.map(|s| Some((module_request, s)))
          }
          .boxed_local()
        };
        inner.pending.push(load_fut);
        inner.state = LoadState::LoadingRoot;
        inner.try_poll_next_unpin(cx)
      }
      LoadState::LoadingRoot | LoadState::LoadingImports => {
        match inner.pending.try_poll_next_unpin(cx)? {
          Poll::Ready(None) => unreachable!(),
          Poll::Ready(Some(None)) => {
            if inner.pending.is_empty() {
              inner.state = LoadState::Done;
              Poll::Ready(None)
            } else {
              Poll::Pending
            }
          }
          Poll::Ready(Some(Some(info))) => Poll::Ready(Some(Ok(info))),
          Poll::Pending => Poll::Pending,
        }
      }
      LoadState::Done => Poll::Ready(None),
    }
  }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[repr(u8)]
pub(crate) enum AssertedModuleType {
  /// JavaScript or WASM.
  JavaScriptOrWasm,
  /// JSON.
  Json,

  // IMPORTANT: If you add any additional enum values here, you must update `to_v8`` below!
  /// Non-well-known module type.
  Other(Cow<'static, str>),
}

impl AssertedModuleType {
  pub fn to_v8<'s>(
    &self,
    scope: &mut v8::HandleScope<'s>,
  ) -> v8::Local<'s, v8::Value> {
    match self {
      AssertedModuleType::JavaScriptOrWasm => v8::Integer::new(scope, 0).into(),
      AssertedModuleType::Json => v8::Integer::new(scope, 1).into(),
      AssertedModuleType::Other(ty) => {
        v8::String::new(scope, ty).unwrap().into()
      }
    }
  }

  pub fn try_from_v8(
    scope: &mut v8::HandleScope,
    value: v8::Local<v8::Value>,
  ) -> Option<Self> {
    Some(if let Some(int) = value.to_integer(scope) {
      match int.int32_value(scope).unwrap_or_default() {
        0 => AssertedModuleType::JavaScriptOrWasm,
        1 => AssertedModuleType::Json,
        _ => return None,
      }
    } else if let Ok(str) = v8::Local::<v8::String>::try_from(value) {
      AssertedModuleType::Other(Cow::Owned(str.to_rust_string_lossy(scope)))
    } else {
      return None;
    })
  }
}

impl AsRef<AssertedModuleType> for AssertedModuleType {
  fn as_ref(&self) -> &AssertedModuleType {
    self
  }
}

impl PartialEq<ModuleType> for AssertedModuleType {
  fn eq(&self, other: &ModuleType) -> bool {
    match other {
      ModuleType::JavaScript => self == &AssertedModuleType::JavaScriptOrWasm,
      ModuleType::Json => self == &AssertedModuleType::Json,
    }
  }
}

impl From<ModuleType> for AssertedModuleType {
  fn from(module_type: ModuleType) -> AssertedModuleType {
    match module_type {
      ModuleType::JavaScript => AssertedModuleType::JavaScriptOrWasm,
      ModuleType::Json => AssertedModuleType::Json,
    }
  }
}

impl std::fmt::Display for AssertedModuleType {
  fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
    match self {
      Self::JavaScriptOrWasm => write!(f, "JavaScriptOrWasm"),
      Self::Json => write!(f, "JSON"),
      Self::Other(ty) => write!(f, "Other({ty})"),
    }
  }
}

/// Describes a request for a module as parsed from the source code.
/// Usually executable (`JavaScriptOrWasm`) is used, except when an
/// import assertions explicitly constrains an import to JSON, in
/// which case this will have a `AssertedModuleType::Json`.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub(crate) struct ModuleRequest {
  pub specifier: String,
  pub asserted_module_type: AssertedModuleType,
}

#[derive(Debug, PartialEq)]
pub(crate) struct ModuleInfo {
  #[allow(unused)]
  pub id: ModuleId,
  pub main: bool,
  pub name: ModuleName,
  pub requests: Vec<ModuleRequest>,
  pub module_type: AssertedModuleType,
}

#[derive(Debug)]
pub(crate) enum ModuleError {
  Exception(v8::Global<v8::Value>),
  Other(Error),
}
