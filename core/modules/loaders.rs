// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
use crate::error::JsErrorClass;
use crate::error::JsNativeError;
use crate::module_specifier::ModuleSpecifier;
use crate::modules::IntoModuleCodeString;
use crate::modules::ModuleCodeString;
use crate::modules::ModuleName;
use crate::modules::ModuleSource;
use crate::modules::ModuleSourceFuture;
use crate::modules::ModuleType;
use crate::modules::RequestedModuleType;
use crate::modules::ResolutionKind;
use crate::resolve_import;
use crate::ModuleSourceCode;

use anyhow::Context;
use deno_core::error::CoreError;
use futures::future::FutureExt;
use std::cell::RefCell;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;

#[derive(Debug, thiserror::Error)]
pub enum ModuleLoaderError {
  #[error("Specifier \"{0}\" was not passed as an extension module and was not included in the snapshot."
  )]
  SpecifierExcludedFromSnapshot(ModuleSpecifier),
  #[error("Specifier \"{0}\" cannot be lazy-loaded as it was not included in the binary."
  )]
  SpecifierMissingLazyLoadable(ModuleSpecifier),
  #[error(
    "\"npm:\" specifiers are currently not supported in import.meta.resolve()"
  )]
  NpmUnsupportedMetaResolve,
  #[error("Attempted to load JSON module without specifying \"type\": \"json\" attribute in the import statement."
  )]
  JsonMissingAttribute,
  #[error("Module not found")]
  NotFound,
  #[error(
    "Module loading is not supported; attempted to load: \"{specifier}\" from \"{}\"",
    .maybe_referrer.as_ref().map_or("(no referrer)", |referrer| referrer.as_str())
  )]
  Unsupported {
    specifier: Box<ModuleSpecifier>,
    maybe_referrer: Option<Box<ModuleSpecifier>>,
  },
  #[error(transparent)]
  Resolution(#[from] crate::ModuleResolutionError),
  #[error(transparent)]
  Core(#[from] CoreError),
}

impl JsErrorClass for ModuleLoaderError {
  fn get_class(&self) -> &'static str {
    match self {
      ModuleLoaderError::SpecifierExcludedFromSnapshot(_) => "Error",
      ModuleLoaderError::SpecifierMissingLazyLoadable(_) => "Error",
      ModuleLoaderError::NpmUnsupportedMetaResolve => "Error",
      ModuleLoaderError::JsonMissingAttribute => "Error",
      ModuleLoaderError::NotFound => "Error",
      ModuleLoaderError::Unsupported { .. } => "Error",
      ModuleLoaderError::Resolution(err) => err.get_class(),
      ModuleLoaderError::Core(err) => err.get_class(),
    }
  }
}

impl From<anyhow::Error> for ModuleLoaderError {
  fn from(err: anyhow::Error) -> Self {
    ModuleLoaderError::Core(CoreError::Other(err))
  }
}

impl From<std::io::Error> for ModuleLoaderError {
  fn from(err: std::io::Error) -> Self {
    ModuleLoaderError::Core(CoreError::Io(err))
  }
}
impl From<JsNativeError> for ModuleLoaderError {
  fn from(err: JsNativeError) -> Self {
    ModuleLoaderError::Core(CoreError::JsNative(err))
  }
}

/// Result of calling `ModuleLoader::load`.
pub enum ModuleLoadResponse {
  /// Source file is available synchronously - eg. embedder might have
  /// collected all the necessary sources in `ModuleLoader::prepare_module_load`.
  /// Slightly cheaper than `Async` as it avoids boxing.
  Sync(Result<ModuleSource, ModuleLoaderError>),

  /// Source file needs to be loaded. Requires boxing due to recrusive
  /// nature of module loading.
  Async(Pin<Box<ModuleSourceFuture>>),
}

pub trait ModuleLoader {
  /// Returns an absolute URL.
  /// When implementing an spec-complaint VM, this should be exactly the
  /// algorithm described here:
  /// <https://html.spec.whatwg.org/multipage/webappapis.html#resolve-a-module-specifier>
  ///
  /// [`ResolutionKind::MainModule`] can be used to resolve from current working directory or
  /// apply import map for child imports.
  ///
  /// [`ResolutionKind::DynamicImport`] can be used to check permissions or deny
  /// dynamic imports altogether.
  fn resolve(
    &self,
    specifier: &str,
    referrer: &str,
    kind: ResolutionKind,
  ) -> Result<ModuleSpecifier, ModuleLoaderError>;

  /// Given ModuleSpecifier, load its source code.
  ///
  /// `is_dyn_import` can be used to check permissions or deny
  /// dynamic imports altogether.
  fn load(
    &self,
    module_specifier: &ModuleSpecifier,
    maybe_referrer: Option<&ModuleSpecifier>,
    is_dyn_import: bool,
    requested_module_type: RequestedModuleType,
  ) -> ModuleLoadResponse;

  /// This hook can be used by implementors to do some preparation
  /// work before starting loading of modules.
  ///
  /// For example implementor might download multiple modules in
  /// parallel and transpile them to final JS sources before
  /// yielding control back to the runtime.
  ///
  /// It's not required to implement this method.
  fn prepare_load(
    &self,
    _module_specifier: &ModuleSpecifier,
    _maybe_referrer: Option<String>,
    _is_dyn_import: bool,
  ) -> Pin<Box<dyn Future<Output = Result<(), ModuleLoaderError>>>> {
    async { Ok(()) }.boxed_local()
  }

  /// Called when new v8 code cache is available for this module. Implementors
  /// can store the provided code cache for future executions of the same module.
  ///
  /// It's not required to implement this method.
  fn code_cache_ready(
    &self,
    _module_specifier: ModuleSpecifier,
    _hash: u64,
    _code_cache: &[u8],
  ) -> Pin<Box<dyn Future<Output = ()>>> {
    async {}.boxed_local()
  }

  /// Called when V8 code cache should be ignored for this module. This can happen
  /// if eg. module causes a V8 warning, like when using deprecated import assertions.
  /// Implementors should make sure that the code cache for this module is purged and not saved anymore.
  ///
  /// It's not required to implement this method.
  fn purge_and_prevent_code_cache(&self, _module_specifier: &str) {}

  /// Returns a source map for given `file_name`.
  ///
  /// This function will soon be deprecated or renamed.
  fn get_source_map(&self, _file_name: &str) -> Option<Vec<u8>> {
    None
  }

  fn get_source_mapped_source_line(
    &self,
    _file_name: &str,
    _line_number: usize,
  ) -> Option<String> {
    None
  }

  /// Implementors can attach arbitrary data to scripts and modules
  /// by implementing this method. V8 currently requires that the
  /// returned data be a `v8::PrimitiveArray`.
  fn get_host_defined_options<'s>(
    &self,
    _scope: &mut v8::HandleScope<'s>,
    _name: &str,
  ) -> Option<v8::Local<'s, v8::Data>> {
    None
  }
}

/// Placeholder structure used when creating
/// a runtime that doesn't support module loading.
pub struct NoopModuleLoader;

impl ModuleLoader for NoopModuleLoader {
  fn resolve(
    &self,
    specifier: &str,
    referrer: &str,
    _kind: ResolutionKind,
  ) -> Result<ModuleSpecifier, ModuleLoaderError> {
    Ok(resolve_import(specifier, referrer)?)
  }

  fn load(
    &self,
    module_specifier: &ModuleSpecifier,
    maybe_referrer: Option<&ModuleSpecifier>,
    _is_dyn_import: bool,
    _requested_module_type: RequestedModuleType,
  ) -> ModuleLoadResponse {
    ModuleLoadResponse::Sync(Err(ModuleLoaderError::Unsupported {
      specifier: Box::new(module_specifier.clone()),
      maybe_referrer: maybe_referrer.map(|referrer| Box::new(referrer.clone())),
    }))
  }
}

pub(crate) struct ExtModuleLoader {
  sources: RefCell<HashMap<ModuleName, ModuleCodeString>>,
}

impl ExtModuleLoader {
  pub fn new(loaded_sources: Vec<(ModuleName, ModuleCodeString)>) -> Self {
    // Guesstimate a length
    let mut sources = HashMap::with_capacity(loaded_sources.len());
    for source in loaded_sources {
      sources.insert(source.0, source.1);
    }
    ExtModuleLoader {
      sources: RefCell::new(sources),
    }
  }

  pub fn finalize(self) -> Result<(), CoreError> {
    let sources = self.sources.take();
    let unused_modules: Vec<_> = sources.iter().collect();

    if !unused_modules.is_empty() {
      return Err(CoreError::UnusedModules(
        unused_modules
          .into_iter()
          .map(|(name, _)| name.to_string())
          .collect::<Vec<_>>(),
      ));
    }

    Ok(())
  }
}

impl ModuleLoader for ExtModuleLoader {
  fn resolve(
    &self,
    specifier: &str,
    referrer: &str,
    _kind: ResolutionKind,
  ) -> Result<ModuleSpecifier, ModuleLoaderError> {
    // If specifier is relative to an extension module, we need to do some special handling
    if specifier.starts_with("../")
      || specifier.starts_with("./")
      || referrer.starts_with("ext:")
    {
      // add `/` to the referrer to make it a valid base URL, so we can join the specifier to it
      return Ok(crate::resolve_url(
        &crate::resolve_url(referrer.replace("ext:", "ext:/").as_str())?
          .join(specifier)
          .map_err(crate::ModuleResolutionError::InvalidBaseUrl)?
          .as_str()
          // remove the `/` we added
          .replace("ext:/", "ext:"),
      )?);
    }
    Ok(resolve_import(specifier, referrer)?)
  }

  fn load(
    &self,
    specifier: &ModuleSpecifier,
    _maybe_referrer: Option<&ModuleSpecifier>,
    _is_dyn_import: bool,
    _requested_module_type: RequestedModuleType,
  ) -> ModuleLoadResponse {
    let mut sources = self.sources.borrow_mut();
    let source = match sources.remove(specifier.as_str()) {
      Some(source) => source,
      None => {
        return ModuleLoadResponse::Sync(Err(
          ModuleLoaderError::SpecifierExcludedFromSnapshot(
            specifier.to_owned(),
          ),
        ))
      }
    };
    ModuleLoadResponse::Sync(Ok(ModuleSource::new(
      ModuleType::JavaScript,
      ModuleSourceCode::String(source),
      specifier,
      None,
    )))
  }

  fn prepare_load(
    &self,
    _specifier: &ModuleSpecifier,
    _maybe_referrer: Option<String>,
    _is_dyn_import: bool,
  ) -> Pin<Box<dyn Future<Output = Result<(), ModuleLoaderError>>>> {
    async { Ok(()) }.boxed_local()
  }
}

/// A loader that is used in `op_lazy_load_esm` to load and execute
/// ES modules that were embedded in the binary using `lazy_loaded_esm`
/// option in `extension!` macro.
pub(crate) struct LazyEsmModuleLoader {
  sources: Rc<RefCell<HashMap<ModuleName, ModuleCodeString>>>,
}

impl LazyEsmModuleLoader {
  pub fn new(
    sources: Rc<RefCell<HashMap<ModuleName, ModuleCodeString>>>,
  ) -> Self {
    LazyEsmModuleLoader { sources }
  }
}

impl ModuleLoader for LazyEsmModuleLoader {
  fn resolve(
    &self,
    specifier: &str,
    referrer: &str,
    _kind: ResolutionKind,
  ) -> Result<ModuleSpecifier, ModuleLoaderError> {
    Ok(resolve_import(specifier, referrer)?)
  }

  fn load(
    &self,
    specifier: &ModuleSpecifier,
    _maybe_referrer: Option<&ModuleSpecifier>,
    _is_dyn_import: bool,
    _requested_module_type: RequestedModuleType,
  ) -> ModuleLoadResponse {
    let mut sources = self.sources.borrow_mut();
    let source = match sources.remove(specifier.as_str()) {
      Some(source) => source,
      None => {
        return ModuleLoadResponse::Sync(Err(
          ModuleLoaderError::SpecifierMissingLazyLoadable(specifier.clone()),
        ))
      }
    };
    ModuleLoadResponse::Sync(Ok(ModuleSource::new(
      ModuleType::JavaScript,
      ModuleSourceCode::String(source),
      specifier,
      None,
    )))
  }

  fn prepare_load(
    &self,
    _specifier: &ModuleSpecifier,
    _maybe_referrer: Option<String>,
    _is_dyn_import: bool,
  ) -> Pin<Box<dyn Future<Output = Result<(), ModuleLoaderError>>>> {
    async { Ok(()) }.boxed_local()
  }
}

/// Basic file system module loader.
///
/// Note that this loader will **block** event loop
/// when loading file as it uses synchronous FS API
/// from standard library.
pub struct FsModuleLoader;

impl ModuleLoader for FsModuleLoader {
  fn resolve(
    &self,
    specifier: &str,
    referrer: &str,
    _kind: ResolutionKind,
  ) -> Result<ModuleSpecifier, ModuleLoaderError> {
    Ok(resolve_import(specifier, referrer)?)
  }

  fn load(
    &self,
    module_specifier: &ModuleSpecifier,
    _maybe_referrer: Option<&ModuleSpecifier>,
    _is_dynamic: bool,
    requested_module_type: RequestedModuleType,
  ) -> ModuleLoadResponse {
    let module_specifier = module_specifier.clone();
    let fut = async move {
      let path = module_specifier.to_file_path().map_err(|_| {
        JsNativeError::generic(format!(
          "Provided module specifier \"{module_specifier}\" is not a file URL."
        ))
      })?;
      let module_type = if let Some(extension) = path.extension() {
        let ext = extension.to_string_lossy().to_lowercase();
        // We only return JSON modules if extension was actually `.json`.
        // In other cases we defer to actual requested module type, so runtime
        // can decide what to do with it.
        if ext == "json" {
          ModuleType::Json
        } else {
          match &requested_module_type {
            RequestedModuleType::Other(ty) => ModuleType::Other(ty.clone()),
            _ => ModuleType::JavaScript,
          }
        }
      } else {
        ModuleType::JavaScript
      };

      // If we loaded a JSON file, but the "requested_module_type" (that is computed from
      // import attributes) is not JSON we need to fail.
      if module_type == ModuleType::Json
        && requested_module_type != RequestedModuleType::Json
      {
        return Err(ModuleLoaderError::JsonMissingAttribute);
      }

      let code = std::fs::read(path).with_context(|| {
        format!("Failed to load {}", module_specifier.as_str())
      })?;
      let module = ModuleSource::new(
        module_type,
        ModuleSourceCode::Bytes(code.into_boxed_slice().into()),
        &module_specifier,
        None,
      );
      Ok(module)
    }
    .boxed_local();

    ModuleLoadResponse::Async(fut)
  }
}

/// A module loader that you can pre-load a number of modules into and resolve from. Useful for testing and
/// embedding situations where the filesystem and snapshot systems are not usable or a good fit.
#[derive(Default)]
pub struct StaticModuleLoader {
  map: HashMap<ModuleSpecifier, ModuleCodeString>,
}

impl StaticModuleLoader {
  /// Create a new [`StaticModuleLoader`] from an `Iterator` of specifiers and code.
  pub fn new(
    from: impl IntoIterator<Item = (ModuleSpecifier, impl IntoModuleCodeString)>,
  ) -> Self {
    Self {
      map: HashMap::from_iter(
        from.into_iter().map(|(url, code)| {
          (url, code.into_module_code().into_cheap_copy().0)
        }),
      ),
    }
  }

  /// Create a new [`StaticModuleLoader`] from a single code item.
  pub fn with(
    specifier: ModuleSpecifier,
    code: impl IntoModuleCodeString,
  ) -> Self {
    Self::new([(specifier, code)])
  }
}

impl ModuleLoader for StaticModuleLoader {
  fn resolve(
    &self,
    specifier: &str,
    referrer: &str,
    _kind: ResolutionKind,
  ) -> Result<ModuleSpecifier, ModuleLoaderError> {
    Ok(resolve_import(specifier, referrer)?)
  }

  fn load(
    &self,
    module_specifier: &ModuleSpecifier,
    _maybe_referrer: Option<&ModuleSpecifier>,
    _is_dyn_import: bool,
    _requested_module_type: RequestedModuleType,
  ) -> ModuleLoadResponse {
    let res = if let Some(code) = self.map.get(module_specifier) {
      Ok(ModuleSource::new(
        ModuleType::JavaScript,
        ModuleSourceCode::String(code.try_clone().unwrap()),
        module_specifier,
        None,
      ))
    } else {
      Err(ModuleLoaderError::NotFound)
    };
    ModuleLoadResponse::Sync(res)
  }
}

/// Annotates a `ModuleLoader` with a log of all `load()` calls.
/// as well as a count of all `resolve()`, `prepare()`, and `load()` calls.
#[cfg(test)]
pub struct TestingModuleLoader<L: ModuleLoader> {
  loader: L,
  log: RefCell<Vec<ModuleSpecifier>>,
  load_count: std::cell::Cell<usize>,
  prepare_count: std::cell::Cell<usize>,
  resolve_count: std::cell::Cell<usize>,
}

#[cfg(test)]
impl<L: ModuleLoader> TestingModuleLoader<L> {
  pub fn new(loader: L) -> Self {
    Self {
      loader,
      log: RefCell::new(vec![]),
      load_count: Default::default(),
      prepare_count: Default::default(),
      resolve_count: Default::default(),
    }
  }

  /// Retrieve the current module load event counts.
  pub fn counts(&self) -> ModuleLoadEventCounts {
    ModuleLoadEventCounts {
      load: self.load_count.get(),
      prepare: self.prepare_count.get(),
      resolve: self.resolve_count.get(),
    }
  }
}

#[cfg(test)]
impl<L: ModuleLoader> ModuleLoader for TestingModuleLoader<L> {
  fn resolve(
    &self,
    specifier: &str,
    referrer: &str,
    kind: ResolutionKind,
  ) -> Result<ModuleSpecifier, ModuleLoaderError> {
    self.resolve_count.set(self.resolve_count.get() + 1);
    self.loader.resolve(specifier, referrer, kind)
  }

  fn prepare_load(
    &self,
    module_specifier: &ModuleSpecifier,
    maybe_referrer: Option<String>,
    is_dyn_import: bool,
  ) -> Pin<Box<dyn Future<Output = Result<(), ModuleLoaderError>>>> {
    self.prepare_count.set(self.prepare_count.get() + 1);
    self
      .loader
      .prepare_load(module_specifier, maybe_referrer, is_dyn_import)
  }

  fn load(
    &self,
    module_specifier: &ModuleSpecifier,
    maybe_referrer: Option<&ModuleSpecifier>,
    is_dyn_import: bool,
    requested_module_type: RequestedModuleType,
  ) -> ModuleLoadResponse {
    self.load_count.set(self.load_count.get() + 1);
    self.log.borrow_mut().push(module_specifier.clone());
    self.loader.load(
      module_specifier,
      maybe_referrer,
      is_dyn_import,
      requested_module_type,
    )
  }
}

#[cfg(test)]
#[derive(Copy, Clone, Default, Debug, Eq, PartialEq)]
pub struct ModuleLoadEventCounts {
  pub resolve: usize,
  pub prepare: usize,
  pub load: usize,
}

#[cfg(test)]
impl ModuleLoadEventCounts {
  pub fn new(resolve: usize, prepare: usize, load: usize) -> Self {
    Self {
      resolve,
      prepare,
      load,
    }
  }
}
