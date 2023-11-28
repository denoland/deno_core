use proc_macro2::TokenStream;
use proc_macro2::TokenTree;
use proc_macro_rules::rules;
use quote::ToTokens;

use crate::op2::Op2Error;

#[derive(Debug, Default, Eq, PartialEq)]
pub(crate) struct MacroConfig {
  /// Generate a fastcall method (must be fastcall compatible).
  pub fast: bool,
  /// Do not generate a fastcall method (must be fastcall compatible).
  pub nofast: bool,
  /// Use other ops for the fast alternatives, rather than generating one for this op.
  pub fast_alternatives: Vec<String>,
  /// Marks an async function (either `async fn` or `fn -> impl Future`).
  pub r#async: bool,
  /// Marks a lazy async function (async must also be true).
  pub async_lazy: bool,
  /// Marks a deferred async function (async must also be true).
  pub async_deferred: bool,
  /// Marks an op as re-entrant (can safely call other ops).
  pub reentrant: bool,
}

impl MacroConfig {
  fn from_token_trees(
    flags: Vec<TokenTree>,
    args: Vec<Option<Vec<impl ToTokens>>>,
  ) -> Result<Self, Op2Error> {
    let flags = flags.into_iter().zip(args).map(|(flag, args)| {
      if let Some(args) = args {
        let args = args
          .into_iter()
          .map(|arg| arg.to_token_stream().to_string())
          .collect::<Vec<_>>()
          .join(",");
        format!("{}({args})", flag.into_token_stream())
      } else {
        flag.into_token_stream().to_string()
      }
    });
    Self::from_flags(flags)
  }

  /// Parses a list of string flags.
  pub fn from_flags(
    flags: impl IntoIterator<Item = String>,
  ) -> Result<Self, Op2Error> {
    let mut config: MacroConfig = Self::default();
    let flags = flags.into_iter().collect::<Vec<_>>();

    // Ensure that the flags are sorted in alphabetical order for consistency and searchability
    let mut flags_sorted = flags.clone();
    flags_sorted.sort();
    if flags != flags_sorted {
      return Err(Op2Error::ImproperlySortedAttribute(flags_sorted.join(", ")));
    }

    for flag in flags {
      if flag == "fast" {
        config.fast = true;
      } else if flag.starts_with("fast(") {
        let tokens =
          syn::parse_str::<TokenTree>(&flag[4..])?.into_token_stream();
        config.fast_alternatives = std::panic::catch_unwind(|| {
          rules!(tokens => {
            ( ( $( $types:ty ),+ ) ) => {
              types.into_iter().map(|s| s.into_token_stream().to_string().replace(' ', ""))
            }
          })
        })
        .map_err(|_| Op2Error::PatternMatchFailed("attribute", flag))?
        .collect::<Vec<_>>();
      } else if flag == "nofast" {
        config.nofast = true;
      } else if flag == "async" {
        config.r#async = true;
      } else if flag == "async(lazy)" {
        config.r#async = true;
        config.async_lazy = true;
      } else if flag == "async(deferred)" {
        config.r#async = true;
        config.async_deferred = true;
      } else if flag == "reentrant" {
        config.reentrant = true;
      } else {
        return Err(Op2Error::InvalidAttribute(flag));
      }
    }

    // Test for invalid attribute combinations
    if config.fast && config.nofast {
      return Err(Op2Error::InvalidAttributeCombination("fast", "nofast"));
    }
    if config.fast && !config.fast_alternatives.is_empty() {
      return Err(Op2Error::InvalidAttributeCombination("fast", "fast(...)"));
    }
    if config.fast_alternatives.len() > 1 {
      return Err(Op2Error::TooManyFastAlternatives);
    }
    if config.fast
      && (config.r#async && !config.async_lazy && !config.async_deferred)
    {
      return Err(Op2Error::InvalidAttributeCombination("fast", "async"));
    }
    if config.nofast
      && (config.r#async && !config.async_lazy && !config.async_deferred)
    {
      return Err(Op2Error::InvalidAttributeCombination("nofast", "async"));
    }

    Ok(config)
  }

  /// Parses the attribute parameters that `proc_macro` passes to the macro. For `#[op2(arg1, arg2)]`, the
  /// tokens will be `arg1, arg2`.
  pub fn from_tokens(tokens: TokenStream) -> Result<Self, Op2Error> {
    let attr_string = tokens.to_string();

    let config = std::panic::catch_unwind(|| {
      rules!(tokens => {
        () => {
          Ok(MacroConfig::default())
        }
        ( $($flags:tt $( ( $( $args:ty ),* ) )? ),+ ) => {
          Self::from_token_trees(flags, args)
        }
      })
    })
    .map_err(|_| Op2Error::PatternMatchFailed("attribute", attr_string))??;
    Ok(config)
  }

  #[cfg(test)]
  fn from_token_tree(tokens: TokenTree) -> Result<Self, Op2Error> {
    let attr_string = tokens.to_string();

    let config = std::panic::catch_unwind(|| {
      rules!(tokens.into_token_stream() => {
        ( ( $($flags:tt $( ( $( $args:ty ),* ) )? ),+ ) ) => {
          Self::from_token_trees(flags, args)
        }
      })
    })
    .map_err(|_| Op2Error::PatternMatchFailed("attribute", attr_string))??;
    Ok(config)
  }

  /// If the attribute matches #[op2(...)] or #[op2], returns `Some(MacroConfig)`, otherwise returns `None`.
  #[cfg(test)]
  pub fn from_maybe_attribute_tokens(
    tokens: TokenStream,
  ) -> Result<Option<Self>, Op2Error> {
    rules!(tokens => {
      (#[op2]) => {
        Ok(Some(MacroConfig::default()))
      }
      (#[op2 $flags:tt ]) => {
        Ok(Some(MacroConfig::from_token_tree(flags)?))
      }
      (#[$_attr:meta]) => {
        Ok(None)
      }
    })
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use syn::{ItemFn, Meta};

  fn test_parse(s: &str, expected: MacroConfig) {
    let item_fn = syn::parse_str::<ItemFn>(&format!("#[op2{s}] fn x() {{ }}"))
      .expect("Failed to parse function");
    let attr = item_fn.attrs.get(0).unwrap();
    let config =
      MacroConfig::from_maybe_attribute_tokens(attr.into_token_stream())
        .expect("Failed to parse attribute")
        .expect("Attribute was None");
    assert_eq!(expected, config);
    if let Meta::List(list) = &attr.meta {
      let config =
        MacroConfig::from_tokens(list.tokens.clone().into_token_stream())
          .expect("Failed to parse attribute");
      assert_eq!(expected, config);
    } else if let Meta::Path(..) = &attr.meta {
      // Ignored
    } else {
      panic!("Not a list or path");
    }
  }

  #[test]
  fn test_macro_parse() {
    test_parse("", MacroConfig::default());
    test_parse(
      "(async)",
      MacroConfig {
        r#async: true,
        ..Default::default()
      },
    );
    test_parse(
      "(async(lazy))",
      MacroConfig {
        r#async: true,
        async_lazy: true,
        ..Default::default()
      },
    );
    test_parse(
      "(fast(op_other1))",
      MacroConfig {
        fast_alternatives: vec!["op_other1".to_owned()],
        ..Default::default()
      },
    );
    test_parse(
      "(fast(op_generic::<T>))",
      MacroConfig {
        fast_alternatives: vec!["op_generic::<T>".to_owned()],
        ..Default::default()
      },
    );
  }
}
