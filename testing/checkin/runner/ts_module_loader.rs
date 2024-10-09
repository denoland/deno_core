// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::rc::Rc;

use anyhow::bail;
use anyhow::Context;
use anyhow::Error;

use deno_ast::MediaType;
use deno_ast::ParseParams;
use deno_ast::SourceMapOption;
use deno_core::error::AnyError;
use deno_core::resolve_import;
use deno_core::url::Url;
use deno_core::ModuleCodeBytes;
use deno_core::ModuleCodeString;
use deno_core::ModuleLoadResponse;
use deno_core::ModuleLoader;
use deno_core::ModuleName;
use deno_core::ModuleSource;
use deno_core::ModuleSourceCode;
use deno_core::ModuleSpecifier;
use deno_core::ModuleType;
use deno_core::RequestedModuleType;
use deno_core::ResolutionKind;
use deno_core::SourceMapData;

// TODO(bartlomieju): this is duplicated in `core/examples/ts_modules_loader.rs`.
type SourceMapStore = Rc<RefCell<HashMap<String, Vec<u8>>>>;

// TODO(bartlomieju): this is duplicated in `core/examples/ts_modules_loader.rs`.
#[derive(Default)]
pub struct TypescriptModuleLoader {
  source_maps: SourceMapStore,
}

// TODO(bartlomieju): this is duplicated in `core/examples/ts_modules_loader.rs`.
impl ModuleLoader for TypescriptModuleLoader {
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
    requested_module_type: RequestedModuleType,
  ) -> ModuleLoadResponse {
    let source_maps = self.source_maps.clone();
    fn load(
      source_maps: SourceMapStore,
      module_specifier: &ModuleSpecifier,
      requested_module_type: RequestedModuleType,
    ) -> Result<ModuleSource, AnyError> {
      let root = Path::new(env!("CARGO_MANIFEST_DIR"));
      let start = if module_specifier.scheme() == "test" {
        1
      } else {
        0
      };
      let path = if module_specifier.scheme() == "file" {
        module_specifier.to_file_path().unwrap()
      } else {
        root.join(Path::new(&module_specifier.path()[start..]))
      };
      if let RequestedModuleType::Other(type_) = requested_module_type {
        let bytes = fs::read(path)?;
        return Ok(ModuleSource::new(
          ModuleType::Other(type_),
          ModuleSourceCode::Bytes(ModuleCodeBytes::Boxed(bytes.into())),
          module_specifier,
          None,
        ));
      }

      let media_type = MediaType::from_path(&path);
      let (module_type, should_transpile) = match MediaType::from_path(&path) {
        MediaType::JavaScript | MediaType::Mjs | MediaType::Cjs => {
          (ModuleType::JavaScript, false)
        }
        MediaType::Jsx => (ModuleType::JavaScript, true),
        MediaType::TypeScript
        | MediaType::Mts
        | MediaType::Cts
        | MediaType::Dts
        | MediaType::Dmts
        | MediaType::Dcts
        | MediaType::Tsx => (ModuleType::JavaScript, true),
        MediaType::Json => (ModuleType::Json, false),
        _ => {
          if path.extension().unwrap_or_default() == "nocompile" {
            (ModuleType::JavaScript, false)
          } else {
            bail!("Unknown extension {:?}", path.extension());
          }
        }
      };
      let code = std::fs::read_to_string(&path).with_context(|| {
        format!("Trying to load {path:?} for {module_specifier}")
      })?;
      let code = if should_transpile {
        let parsed = deno_ast::parse_module(ParseParams {
          specifier: module_specifier.clone(),
          text: code.into(),
          media_type,
          capture_tokens: false,
          scope_analysis: false,
          maybe_syntax: None,
        })?;
        let res = parsed.transpile(
          &deno_ast::TranspileOptions {
            imports_not_used_as_values:
              deno_ast::ImportsNotUsedAsValues::Remove,
            use_decorators_proposal: true,
            ..Default::default()
          },
          &deno_ast::EmitOptions {
            source_map: SourceMapOption::Separate,
            inline_sources: false,
            ..Default::default()
          },
        )?;
        let res = res.into_source();
        let source_map = res.source_map.unwrap();
        source_maps
          .borrow_mut()
          .insert(module_specifier.to_string(), source_map);
        String::from_utf8(res.source).unwrap()
      } else {
        code
      };
      Ok(ModuleSource::new(
        module_type,
        ModuleSourceCode::String(code.into()),
        module_specifier,
        None,
      ))
    }

    ModuleLoadResponse::Sync(load(
      source_maps,
      module_specifier,
      requested_module_type,
    ))
  }

  fn get_source_map(&self, specifier: &str) -> Option<Vec<u8>> {
    self.source_maps.borrow().get(specifier).cloned()
  }
}

pub fn maybe_transpile_source(
  specifier: ModuleName,
  source: ModuleCodeString,
) -> Result<(ModuleCodeString, Option<SourceMapData>), AnyError> {
  // Always transpile `checkin:` built-in modules, since they might be TypeScript.
  let media_type = if specifier.starts_with("checkin:") {
    MediaType::TypeScript
  } else {
    MediaType::from_path(Path::new(&specifier))
  };

  match media_type {
    MediaType::TypeScript => {}
    MediaType::JavaScript => return Ok((source, None)),
    MediaType::Mjs => return Ok((source, None)),
    _ => panic!(
      "Unsupported media type for snapshotting {media_type:?} for file {}",
      specifier
    ),
  }

  let parsed = deno_ast::parse_module(ParseParams {
    specifier: Url::parse(&specifier).unwrap(),
    text: source.as_str().into(),
    media_type,
    capture_tokens: false,
    scope_analysis: false,
    maybe_syntax: None,
  })?;
  let transpiled_source = parsed
    .transpile(
      &deno_ast::TranspileOptions {
        imports_not_used_as_values: deno_ast::ImportsNotUsedAsValues::Remove,
        use_decorators_proposal: true,
        ..Default::default()
      },
      &deno_ast::EmitOptions {
        source_map: SourceMapOption::Separate,
        inline_sources: false,
        ..Default::default()
      },
    )?
    .into_source();

  Ok((
    String::from_utf8(transpiled_source.source).unwrap().into(),
    transpiled_source.source_map.map(|s| s.into()),
  ))
}
