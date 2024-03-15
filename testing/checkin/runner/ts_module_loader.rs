// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;

use anyhow::bail;
use anyhow::Error;

use deno_ast::MediaType;
use deno_ast::ParseParams;
use deno_ast::SourceTextInfo;
use deno_core::error::AnyError;
use deno_core::resolve_import;
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
use deno_core::SourceMapGetter;

#[derive(Clone, Default)]
struct SourceMapStore(Rc<RefCell<HashMap<String, Vec<u8>>>>);

impl SourceMapGetter for SourceMapStore {
  fn get_source_map(&self, specifier: &str) -> Option<Vec<u8>> {
    self.0.borrow().get(specifier).cloned()
  }

  fn get_source_line(
    &self,
    _file_name: &str,
    _line_number: usize,
  ) -> Option<String> {
    None
  }
}

#[derive(Default)]
pub struct TypescriptModuleLoader {
  source_maps: SourceMapStore,
}

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
    _requested_module_type: RequestedModuleType,
  ) -> ModuleLoadResponse {
    let source_maps = self.source_maps.clone();
    fn load(
      source_maps: SourceMapStore,
      module_specifier: &ModuleSpecifier,
    ) -> Result<ModuleSource, AnyError> {
      let root = Path::new(env!("CARGO_MANIFEST_DIR"));
      let path = root.join(Path::new(&module_specifier.path()[1..]));

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

      let code = std::fs::read_to_string(&path)?;
      let code = if should_transpile {
        let parsed = deno_ast::parse_module(ParseParams {
          specifier: module_specifier.to_string(),
          text_info: SourceTextInfo::from_string(code),
          media_type,
          capture_tokens: false,
          scope_analysis: false,
          maybe_syntax: None,
        })?;
        let res = parsed.transpile(&deno_ast::EmitOptions {
          inline_source_map: false,
          source_map: true,
          inline_sources: true,
          ..Default::default()
        })?;
        let source_map = res.source_map.unwrap();
        source_maps
          .0
          .borrow_mut()
          .insert(module_specifier.to_string(), source_map.into_bytes());
        res.text
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

    ModuleLoadResponse::Sync(load(source_maps, module_specifier))
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
    specifier: specifier.to_string(),
    text_info: SourceTextInfo::from_string(source.as_str().to_owned()),
    media_type,
    capture_tokens: false,
    scope_analysis: false,
    maybe_syntax: None,
  })?;
  let transpiled_source = parsed.transpile(&deno_ast::EmitOptions {
    imports_not_used_as_values: deno_ast::ImportsNotUsedAsValues::Remove,
    inline_source_map: false,
    source_map: true,
    inline_sources: true,
    ..Default::default()
  })?;

  Ok((
    transpiled_source.text.into(),
    transpiled_source.source_map.map(|s| s.into_bytes().into()),
  ))
}
