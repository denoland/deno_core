// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use crate::fast_string::FastString;
use crate::module_specifier::ModuleSpecifier;
use anyhow::bail;
use anyhow::Error;
use serde::Deserialize;
use serde::Serialize;
use std::borrow::Cow;
use std::collections::HashMap;
use std::future::Future;

mod loaders;
mod map;
mod module_map_data;
mod recursive_load;

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

/// Callback to customize value of `import.meta.resolve("./foo.ts")`.
pub type ImportMetaResolveCallback = Box<
  dyn Fn(&dyn ModuleLoader, String, String) -> Result<ModuleSpecifier, Error>,
>;

pub(crate) fn default_import_meta_resolve_cb(
  loader: &dyn ModuleLoader,
  specifier: String,
  referrer: String,
) -> Result<ModuleSpecifier, Error> {
  if specifier.starts_with("npm:") {
    bail!("\"npm:\" specifiers are currently not supported in import.meta.resolve()");
  }

  loader.resolve(&specifier, &referrer, ResolutionKind::DynamicImport)
}

/// Callback to validate import attributes. If the validation fails and exception
/// should be thrown using `scope.throw_exception()`.
pub type ValidateImportAttributesCb =
  Box<dyn Fn(&mut v8::HandleScope, &HashMap<String, String>)>;

const SUPPORTED_TYPE_ASSERTIONS: &[&str] =
  &["json", "text", "url", "buffer", "css-module"];

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
      } else if ty == "text" {
        AssertedModuleType::Text
      } else if ty == "url" {
        AssertedModuleType::Url
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
  Text,
  Url,
  Buffer,
  CssModule,
}

impl std::fmt::Display for ModuleType {
  fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
    match self {
      Self::JavaScript => write!(f, "JavaScript"),
      Self::Json => write!(f, "JSON"),
      Self::Text => write!(f, "Text"),
      Self::Url => write!(f, "Url"),
      Self::Buffer => write!(f, "Buffer"),
      Self::CssModule => write!(f, "CssModule"),
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

pub type ModuleSourceFuture = dyn Future<Output = Result<ModuleSource, Error>>;

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

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[repr(u8)]
pub(crate) enum AssertedModuleType {
  /// JavaScript or WASM.
  JavaScriptOrWasm,
  /// JSON.
  Json,

  Text,
  Url,
  // Buffer,
  CssModule,

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
      AssertedModuleType::Text => v8::Integer::new(scope, 2).into(),
      AssertedModuleType::Url => v8::Integer::new(scope, 3).into(),
      // AssertedModuleType::Buffer => v8::Integer::new(scope, 4).into(),
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
        2 => AssertedModuleType::Text,
        3 => AssertedModuleType::Url,
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
      ModuleType::Text => self == &AssertedModuleType::Text,
      ModuleType::Url => self == &AssertedModuleType::Url,
      ModuleType::Buffer => todo!(),
    }
  }
}

impl From<ModuleType> for AssertedModuleType {
  fn from(module_type: ModuleType) -> AssertedModuleType {
    match module_type {
      ModuleType::JavaScript => AssertedModuleType::JavaScriptOrWasm,
      ModuleType::Json => AssertedModuleType::Json,
      ModuleType::Text => AssertedModuleType::Text,
      ModuleType::Url => AssertedModuleType::Url,
      ModuleType::Buffer => todo!(),
    }
  }
}

impl std::fmt::Display for AssertedModuleType {
  fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
    match self {
      Self::JavaScriptOrWasm => write!(f, "JavaScriptOrWasm"),
      Self::Json => write!(f, "JSON"),
      Self::Text => write!(f, "text"),
      Self::Url => write!(f, "url"),
      Self::CssModule => write!(f, "css-module"),
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
