// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.

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
  T: SourceMapGetter,
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

#[derive(Debug, Default)]
pub struct SourceMapCache {
  maps: HashMap<String, Option<SourceMap>>,
  source_lines: HashMap<(String, i64), Option<String>>,
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

/// Apply a source map to the passed location. If there is no source map for
/// this location, or if the location remains unchanged after mapping, the
/// changed values are returned.
///
/// Line and column numbers are 1-based.
pub fn apply_source_map<G: SourceMapGetter + ?Sized>(
  file_name: &str,
  line_number: u32,
  column_number: u32,
  cache: &mut SourceMapCache,
  getter: &G,
) -> SourceMapApplication {
  // Lookup expects 0-based line and column numbers, but ours are 1-based.
  let line_number = line_number - 1;
  let column_number = column_number - 1;

  let maybe_source_map_entry = cache.maps.get(file_name);
  let maybe_source_map = maybe_source_map_entry
    .map(Cow::Borrowed)
    .unwrap_or_else(|| {
      let maybe_source_map = getter
        .get_source_map(file_name)
        .and_then(|raw_source_map| SourceMap::from_slice(&raw_source_map).ok());
      Cow::Owned(maybe_source_map)
    });

  let Some(source_map) = maybe_source_map.as_ref() else {
    return SourceMapApplication::Unchanged;
  };

  let Some(token) = source_map.lookup_token(line_number, column_number) else {
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

  match maybe_source_map {
    Cow::Borrowed(_) => {}
    Cow::Owned(maybe_source_map) => {
      cache.maps.insert(file_name.to_owned(), maybe_source_map);
    }
  }

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

pub fn get_source_line<G: SourceMapGetter + ?Sized>(
  file_name: &str,
  line_number: i64,
  cache: &mut SourceMapCache,
  getter: &G,
) -> Option<String> {
  cache
    .source_lines
    .entry((file_name.to_string(), line_number))
    .or_insert_with(|| {
      // Source lookup expects a 0-based line number, ours are 1-based.
      let s = getter.get_source_line(file_name, (line_number - 1) as usize);
      s.filter(|s| s.len() <= MAX_SOURCE_LINE_LENGTH)
    })
    .clone()
}
