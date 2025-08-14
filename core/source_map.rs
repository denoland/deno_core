// Copyright 2018-2025 the Deno authors. MIT license.

//! This mod provides functions to remap a `JsError` based on a source map.

use crate::ModuleLoader;
use crate::ModuleLoader;
use crate::ModuleName;
use crate::ModuleName;
use crate::ModuleResolutionError;
use crate::RequestedModuleType;
use crate::resolve_import;
use crate::resolve_url;
use crate::resolve_url;
use crate::runtime::JsRealm;
use sourcemap::DecodedMap;
pub use sourcemap::SourceMap;
use std::borrow::Cow;
use std::collections::HashMap;
use std::rc::Rc;
use std::str;

const BASE64_PREFIX: &str = "data:application/json;base64,";

#[derive(Debug, PartialEq)]
pub enum SourceMapApplication {
  /// No mapping was applied, the location is unchanged.
  Unchanged,
  /// Line and column were mapped to a new location.
  LineAndColumn {
    line_number: u32,
    column_number: u32,
  },
  /// Line, column and file name were mapped to a new location.
  LineAndColumnAndFileName {
    file_name: String,
    line_number: u32,
    column_number: u32,
  },
}

pub type SourceMapData = Cow<'static, [u8]>;

pub struct SourceMapper {
  // TODO(bartlomieju): I feel like these two should be cleared when Isolate
  // reaches "near heap limit" to free up some space. This needs to be confirmed though.
  maps: HashMap<String, Option<SourceMap>>,
  source_lines: HashMap<(String, i64), Option<String>>,

  loader: Rc<dyn ModuleLoader>,

  ext_source_maps: HashMap<ModuleName, SourceMapData>,
}

impl SourceMapper {
  pub fn new(loader: Rc<dyn ModuleLoader>) -> Self {
    Self {
      maps: Default::default(),
      source_lines: Default::default(),
      ext_source_maps: Default::default(),
      loader,
    }
  }

  /// Add a source map for particular `ext:` module.
  pub(crate) fn add_ext_source_map(
    &mut self,
    module_name: ModuleName,
    source_map_data: SourceMapData,
  ) {
    self.ext_source_maps.insert(module_name, source_map_data);
  }

  pub(crate) fn take_ext_source_maps(
    &mut self,
  ) -> HashMap<ModuleName, SourceMapData> {
    std::mem::take(&mut self.ext_source_maps)
  }

  pub fn apply_source_map_from_module_map(
    &mut self,
    scope: &mut v8::HandleScope,
    file_name: &str,
    line_number: u32,
    column_number: u32,
  ) -> Option<SourceMapApplication> {
    let module_map_rc = JsRealm::module_map_from(scope);
    let id = module_map_rc.get_id(file_name, RequestedModuleType::None)?;

    let module_handle = module_map_rc.get_handle(id).unwrap();
    let module = v8::Local::new(scope, module_handle);
    let unbound_module_script = module.get_unbound_module_script(scope);
    let maybe_source_mapping_url =
      unbound_module_script.get_source_mapping_url(scope);

    if !maybe_source_mapping_url.is_string() {
      return None;
    }

    let source_map_string =
      maybe_source_mapping_url.to_rust_string_lossy(scope);

    // TODO(bartlomieju): this is a fast path - if it fails, we should try to parse
    // the URL (or resolve it from the current file being mapped) and fallback to
    // acquiring a source map from that URL. In Deno we might want to apply permissions
    // checks for fetching the map.
    let source_map = if source_map_string.starts_with(BASE64_PREFIX) {
      let DecodedMap::Regular(sm) =
        sourcemap::decode_data_url(&source_map_string).ok()?
      else {
        return None;
      };
      sm
    } else {
      let url = match resolve_import(&source_map_string, file_name) {
        Ok(url) => Some(url),
        Err(err) => match err {
          ModuleResolutionError::ImportPrefixMissing(_, _) => {
            resolve_import(&format!("./{}", source_map_string), file_name).ok()
          }
          _ => None,
        },
      };
      let url = url?;
      if url.scheme() != "file" {
        return None;
      }
      let source_map_file_name = url.to_file_path().ok()?;
      let source_map_file_name = source_map_file_name.to_str()?;
      let contents = module_map_rc
        .loader
        .borrow()
        .load_source_map_file(source_map_file_name, file_name)?;
      SourceMap::from_slice(&contents).ok()?
    };

    Some(Self::compute_application(
      &source_map,
      file_name,
      line_number,
      column_number,
    ))
  }

  /// Apply a source map to the passed location. If there is no source map for
  /// this location, or if the location remains unchanged after mapping, the
  /// changed values are returned.
  ///
  /// Line and column numbers are 1-based.
  pub fn apply_source_map(
    &mut self,
    scope: &mut v8::HandleScope,
    file_name: &str,
    line_number: u32,
    column_number: u32,
  ) -> SourceMapApplication {
    // Lookup expects 0-based line and column numbers, but ours are 1-based.
    let line_number = line_number - 1;
    let column_number = column_number - 1;

    let maybe_source_map =
      self.maps.entry(file_name.to_owned()).or_insert_with(|| {
        None
          .or_else(|| {
            SourceMap::from_slice(self.ext_source_maps.get(file_name)?).ok()
          })
          .or_else(|| {
            SourceMap::from_slice(
              &self.loader.get_source_map_for_file(file_name)?,
            )
            .ok()
          })
      });

    // If source map is provided externally, return early
    if let Some(source_map) = maybe_source_map.as_ref() {
      return Self::compute_application(
        source_map,
        file_name,
        line_number,
        column_number,
      );
    };

    // Finally fallback to using V8 APIs to discover source map inside the source code of the module.
    if let Some(app) = self.apply_source_map_from_module_map(
      scope,
      file_name,
      line_number,
      column_number,
    ) {
      return app;
    }

    SourceMapApplication::Unchanged
  }

  fn compute_application(
    source_map: &SourceMap,
    file_name: &str,
    line_number: u32,
    column_number: u32,
  ) -> SourceMapApplication {
    let Some(token) = source_map.lookup_token(line_number, column_number)
    else {
      return SourceMapApplication::Unchanged;
    };

    let new_line_number = token.get_src_line() + 1;
    let new_column_number = token.get_src_col() + 1;

    let new_file_name = match token.get_source() {
      Some(source_file_name) => {
        if source_file_name == file_name {
          None
        } else {
          // The `source_file_name` written by tsc in the source map is
          // sometimes only the basename of the URL, or has unwanted `<`/`>`
          // around it. Use the `file_name` we get from V8 if
          // `source_file_name` does not parse as a URL.
          match resolve_url(source_file_name) {
            Ok(m) if m.scheme() == "blob" => None,
            Ok(m) => Some(m.to_string()),
            Err(_) => None,
          }
        }
      }
      None => None,
    };

    match new_file_name {
      None => SourceMapApplication::LineAndColumn {
        line_number: new_line_number,
        column_number: new_column_number,
      },
      Some(file_name) => SourceMapApplication::LineAndColumnAndFileName {
        file_name,
        line_number: new_line_number,
        column_number: new_column_number,
      },
    }
  }

  const MAX_SOURCE_LINE_LENGTH: usize = 150;

  pub fn get_source_line(
    &mut self,
    file_name: &str,
    line_number: i64,
  ) -> Option<String> {
    // eprintln!("get_source_line {} {}", file_name, line_number);
    if let Some(maybe_source_line) =
      self.source_lines.get(&(file_name.to_string(), line_number))
    {
      return maybe_source_line.clone();
    }

    let s = self
      .loader
      .get_source_mapped_source_line(file_name, (line_number - 1) as usize);
    let maybe_source_line =
      s.filter(|s| s.len() <= Self::MAX_SOURCE_LINE_LENGTH);
    // Cache and return
    self.source_lines.insert(
      (file_name.to_string(), line_number),
      maybe_source_line.clone(),
    );
    maybe_source_line
  }
}

#[cfg(test)]
mod tests {
  use url::Url;

  use super::*;
  use crate::JsRuntime;
  use crate::ModuleCodeString;
  use crate::ModuleLoadResponse;
  use crate::ModuleSpecifier;
  use crate::RequestedModuleType;
  use crate::ResolutionKind;
  use crate::RuntimeOptions;
  use crate::ascii_str;
  use crate::ascii_str;
  use crate::error::ModuleLoaderError;

  struct SourceMapLoaderContent {
    source_map: Option<ModuleCodeString>,
  }

  #[derive(Default)]
  pub struct SourceMapLoader {
    map: HashMap<ModuleSpecifier, SourceMapLoaderContent>,
  }

  impl ModuleLoader for SourceMapLoader {
    fn resolve(
      &self,
      _specifier: &str,
      _referrer: &str,
      _kind: ResolutionKind,
    ) -> Result<ModuleSpecifier, ModuleLoaderError> {
      unreachable!()
    }

    fn load(
      &self,
      _module_specifier: &ModuleSpecifier,
      _maybe_referrer: Option<&ModuleSpecifier>,
      _is_dyn_import: bool,
      _requested_module_type: RequestedModuleType,
    ) -> ModuleLoadResponse {
      unreachable!()
    }

    fn get_source_map_for_file(&self, file_name: &str) -> Option<Vec<u8>> {
      let url = Url::parse(file_name).unwrap();
      let content = self.map.get(&url)?;
      content
        .source_map
        .as_ref()
        .map(|s| Cow::Borrowed(s.as_bytes()))
    }

    fn get_source_map(&self, file_name: &str) -> Option<Cow<[u8]>> {
      let url = Url::parse(file_name).unwrap();
      let content = self.map.get(&url)?;
      content
        .source_map
        .as_ref()
        .map(|s| Cow::Borrowed(s.as_bytes()))
    }

    fn load_source_map_file(
      &self,
      _source_map_file_name: &str,
      _file_name: &str,
    ) -> Option<Vec<u8>> {
      todo!()
    }

    fn get_source_mapped_source_line(
      &self,
      _file_name: &str,
      _line_number: usize,
    ) -> Option<String> {
      Some("fake source line".to_string())
    }
  }

  #[test]
  fn test_source_mapper() {
    let mut loader = SourceMapLoader::default();
    loader.map.insert(
      Url::parse("file:///b.js").unwrap(),
      SourceMapLoaderContent { source_map: None },
    );
    loader.map.insert(
      Url::parse("file:///a.ts").unwrap(),
      SourceMapLoaderContent {
        source_map: Some(ascii_str!(r#"{"version":3,"sources":["file:///a.ts"],"sourcesContent":["export function a(): string {\n  return \"a\";\n}\n"],"names":[],"mappings":"AAAA,OAAO,SAAS;EACd,OAAO;AACT"}"#).into()),
      },
    );

    let loader = Rc::new(loader);

    let mut js_runtime = JsRuntime::new(RuntimeOptions {
      module_loader: Some(loader.clone()),
      ..Default::default()
    });
    let state = JsRuntime::state_from(js_runtime.v8_isolate());
    let scope = &mut js_runtime.handle_scope();
    let mut source_mapper = state.source_mapper.borrow_mut();

    // Non-existent file
    let application =
      source_mapper.apply_source_map(scope, "file:///doesnt_exist.js", 1, 1);
    assert_eq!(application, SourceMapApplication::Unchanged);

    // File with no source map
    let application =
      source_mapper.apply_source_map(scope, "file:///b.js", 1, 1);
    assert_eq!(application, SourceMapApplication::Unchanged);

    // File with a source map
    let application =
      source_mapper.apply_source_map(scope, "file:///a.ts", 1, 21);
    assert_eq!(
      application,
      SourceMapApplication::LineAndColumn {
        line_number: 1,
        column_number: 17
      }
    );

    let line = source_mapper.get_source_line("file:///a.ts", 1).unwrap();
    assert_eq!(line, "fake source line");
    // Get again to hit a cache
    let line = source_mapper.get_source_line("file:///a.ts", 1).unwrap();
    assert_eq!(line, "fake source line");
  }
}
