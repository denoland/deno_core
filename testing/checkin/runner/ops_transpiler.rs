// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

use deno_ast::MediaType;
use deno_ast::ParseParams;
use deno_ast::SourceMapOption;
use deno_core::error::AnyError;
use deno_core::op2;
use deno_core::url::Url;

#[op2]
#[serde]
pub fn op_transpile(
  #[string] specifier: &str,
  #[string] source: String,
) -> Result<(String, Option<String>), AnyError> {
  let media_type = MediaType::from_str(specifier);

  let parsed = deno_ast::parse_module(ParseParams {
    specifier: Url::parse(specifier).unwrap(),
    text: source.into(),
    media_type,
    capture_tokens: false,
    scope_analysis: false,
    maybe_syntax: None,
  })?;
  let transpile_result = parsed.transpile(
    &deno_ast::TranspileOptions {
      use_decorators_proposal: true,
      imports_not_used_as_values: deno_ast::ImportsNotUsedAsValues::Remove,
      ..Default::default()
    },
    &deno_ast::EmitOptions {
      source_map: SourceMapOption::Inline,
      inline_sources: false,
      ..Default::default()
    },
  )?;
  let transpiled_source = transpile_result.into_source();

  Ok((
    String::from_utf8(transpiled_source.source).unwrap(),
    transpiled_source
      .source_map
      .map(|s| String::from_utf8(s).unwrap()),
  ))
}
