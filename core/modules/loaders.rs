// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
use crate::error::generic_error;
use crate::extensions::ExtensionFileSource;
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

use anyhow::anyhow;
use anyhow::bail;
use anyhow::Context;
use anyhow::Error;
use futures::future::FutureExt;
use std::cell::RefCell;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;

/// Result of calling `ModuleLoader::load`.
pub enum ModuleLoadResponse {
  /// Source file is available synchronously - eg. embedder might have
  /// collected all the necessary sources in `ModuleLoader::prepare_module_load`.
  /// Slightly cheaper than `Async` as it avoids boxing.
  Sync(Result<ModuleSource, Error>),

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
  ) -> Result<ModuleSpecifier, Error>;

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
    _module_specifiers: &[ModuleSpecifier],
    _maybe_referrer: Option<String>,
    _is_dyn_import: bool,
  ) -> Pin<Box<dyn Future<Output = Result<(), Error>>>> {
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
  ) -> Result<ModuleSpecifier, Error> {
    Ok(resolve_import(specifier, referrer)?)
  }

  fn load(
    &self,
    module_specifier: &ModuleSpecifier,
    maybe_referrer: Option<&ModuleSpecifier>,
    _is_dyn_import: bool,
    _requested_module_type: RequestedModuleType,
  ) -> ModuleLoadResponse {
    let maybe_referrer = maybe_referrer
      .map(|s| s.as_str())
      .unwrap_or("(no referrer)");
    let err = generic_error(
      format!(
        "Module loading is not supported; attempted to load: \"{module_specifier}\" from \"{maybe_referrer}\"",
      )
    );
    ModuleLoadResponse::Sync(Err(err))
  }
}

/// Function that can be passed to the `ExtModuleLoader` that allows to
/// transpile sources before passing to V8.
pub type ExtModuleLoaderCb =
  Box<dyn Fn(&ExtensionFileSource) -> Result<ModuleCodeString, Error>>;

pub(crate) struct ExtModuleLoader {
  sources: RefCell<HashMap<ModuleName, ModuleCodeString>>,
}

impl ExtModuleLoader {
  pub fn new(
    loaded_sources: Vec<(ModuleName, ModuleCodeString)>,
  ) -> Result<Self, Error> {
    // Guesstimate a length
    let mut sources = HashMap::with_capacity(loaded_sources.len());
    for source in loaded_sources {
      sources.insert(source.0, source.1);
    }
    Ok(ExtModuleLoader {
      sources: RefCell::new(sources),
    })
  }

  pub fn finalize(self) -> Result<(), Error> {
    let sources = self.sources.take();
    let unused_modules: Vec<_> = sources.iter().collect();

    if !unused_modules.is_empty() {
      let mut msg =
        "Following modules were passed to ExtModuleLoader but never used:\n"
          .to_string();
      for m in unused_modules {
        msg.push_str("  - ");
        msg.push_str(m.0);
        msg.push('\n');
      }
      bail!(msg);
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
  ) -> Result<ModuleSpecifier, Error> {
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
      None => return ModuleLoadResponse::Sync(Err(anyhow!("Specifier \"{}\" was not passed as an extension module and was not included in the snapshot.", specifier))),
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
    _specifiers: &[ModuleSpecifier],
    _maybe_referrer: Option<String>,
    _is_dyn_import: bool,
  ) -> Pin<Box<dyn Future<Output = Result<(), Error>>>> {
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
  ) -> Result<ModuleSpecifier, Error> {
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
      None => return ModuleLoadResponse::Sync(Err(anyhow!("Specifier \"{}\" cannot be lazy-loaded as it was not included in the binary.", specifier))),
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
    _specifiers: &[ModuleSpecifier],
    _maybe_referrer: Option<String>,
    _is_dyn_import: bool,
  ) -> Pin<Box<dyn Future<Output = Result<(), Error>>>> {
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
  ) -> Result<ModuleSpecifier, Error> {
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
        generic_error(format!(
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
        return Err(generic_error("Attempted to load JSON module without specifying \"type\": \"json\" attribute in the import statement."));
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
  ) -> Result<ModuleSpecifier, Error> {
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
      Err(generic_error("Module not found"))
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
  ) -> Result<ModuleSpecifier, Error> {
    self.resolve_count.set(self.resolve_count.get() + 1);
    self.loader.resolve(specifier, referrer, kind)
  }

  fn prepare_load(
    &self,
    module_specifiers: &[ModuleSpecifier],
    maybe_referrer: Option<String>,
    is_dyn_import: bool,
  ) -> Pin<Box<dyn Future<Output = Result<(), Error>>>> {
    self.prepare_count.set(self.prepare_count.get() + 1);
    self
      .loader
      .prepare_load(module_specifiers, maybe_referrer, is_dyn_import)
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
