// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

//! This mod provides functions to remap a `JsError` based on a source map.

use crate::resolve_url;
pub use sourcemap::SourceMap;
use std::borrow::Cow;
use std::collections::HashMap;
use std::rc::Rc;
use std::str;

pub trait SourceMapGetter {
  /// Returns the raw source map file.
  fn get_source_map(&self, file_name: &str) -> Option<Vec<u8>>;
  fn get_source_line(
    &self,
    file_name: &str,
    line_number: usize,
  ) -> Option<String>;
}

impl<T> SourceMapGetter for Rc<T>
where
  T: SourceMapGetter + ?Sized,
{
  fn get_source_map(&self, file_name: &str) -> Option<Vec<u8>> {
    (**self).get_source_map(file_name)
  }

  fn get_source_line(
    &self,
    file_name: &str,
    line_number: usize,
  ) -> Option<String> {
    (**self).get_source_line(file_name, line_number)
  }
}

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

pub struct SourceMapper<G: SourceMapGetter> {
  maps: HashMap<String, Option<SourceMap>>,
  source_lines: HashMap<(String, i64), Option<String>>,
  getter: Option<G>,
  pub(crate) ext_source_maps: HashMap<String, SourceMapData>,
  // This is not the right place for this, but it's the easiest way to make
  // op_apply_source_map a fast op. This stashing should happen in #[op].
  pub(crate) stashed_file_name: Option<String>,
}

impl<G: SourceMapGetter> SourceMapper<G> {
  pub fn new(getter: Option<G>) -> Self {
    Self {
      maps: Default::default(),
      source_lines: Default::default(),
      ext_source_maps: Default::default(),
      getter,
      stashed_file_name: Default::default(),
    }
  }

  pub fn has_user_sources(&self) -> bool {
    self.getter.is_some()
  }

  /// Apply a source map to the passed location. If there is no source map for
  /// this location, or if the location remains unchanged after mapping, the
  /// changed values are returned.
  ///
  /// Line and column numbers are 1-based.
  pub fn apply_source_map(
    &mut self,
    file_name: &str,
    line_number: u32,
    column_number: u32,
  ) -> SourceMapApplication {
    // Lookup expects 0-based line and column numbers, but ours are 1-based.
    let line_number = line_number - 1;
    let column_number = column_number - 1;

    let getter = self.getter.as_ref();
    let maybe_source_map =
      self.maps.entry(file_name.to_owned()).or_insert_with(|| {
        None
          .or_else(|| {
            SourceMap::from_slice(self.ext_source_maps.get(file_name)?).ok()
          })
          .or_else(|| {
            SourceMap::from_slice(&getter?.get_source_map(file_name)?).ok()
          })
      });

    let Some(source_map) = maybe_source_map.as_ref() else {
      return SourceMapApplication::Unchanged;
    };

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
    let getter = self.getter.as_ref()?;
    self
      .source_lines
      .entry((file_name.to_string(), line_number))
      .or_insert_with(|| {
        // Source lookup expects a 0-based line number, ours are 1-based.
        let s = getter.get_source_line(file_name, (line_number - 1) as usize);
        s.filter(|s| s.len() <= Self::MAX_SOURCE_LINE_LENGTH)
      })
      .clone()
  }
}
