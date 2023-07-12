// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use deno_proc_macro_rules::rules;
use proc_macro2::Ident;
use proc_macro2::Span;
use proc_macro2::TokenStream;
use quote::format_ident;
use quote::quote;
use quote::ToTokens;
use std::iter::zip;
use syn2::parse2;
use syn2::parse_str;
use syn2::spanned::Spanned;
use syn2::FnArg;
use syn2::ItemFn;
use syn2::Lifetime;
use syn2::LifetimeParam;
use syn2::Path;
use thiserror::Error;

use self::dispatch_fast::generate_dispatch_fast;
use self::dispatch_slow::generate_dispatch_slow;
use self::generator_state::GeneratorState;
use self::signature::is_attribute_special;
use self::signature::parse_signature;
use self::signature::Arg;
use self::signature::SignatureError;

pub mod dispatch_fast;
pub mod dispatch_shared;
pub mod dispatch_slow;
pub mod generator_state;
pub mod signature;

#[derive(Debug, Error)]
pub enum Op2Error {
  #[error("Failed to match a pattern for '{0}': (input was '{1}')")]
  PatternMatchFailed(&'static str, String),
  #[error("Invalid attribute: '{0}'")]
  InvalidAttribute(String),
  #[error("Failed to parse syntax tree")]
  ParseError(#[from] syn2::Error),
  #[error("Failed to map a parsed signature to a V8 call")]
  V8MappingError(#[from] V8MappingError),
  #[error("Failed to parse signature")]
  SignatureError(#[from] SignatureError),
  #[error("This op is fast-compatible and should be marked as (fast)")]
  ShouldBeFast,
  #[error("This op is not fast-compatible and should not be marked as (fast)")]
  ShouldNotBeFast,
}

#[derive(Debug, Error)]
pub enum V8MappingError {
  #[error("Unable to map {1:?} to {0}")]
  NoMapping(&'static str, Arg),
}

#[derive(Default)]
pub(crate) struct MacroConfig {
  pub core: bool,
  pub fast: bool,
}

impl MacroConfig {
  pub fn from_flags(flags: Vec<Ident>) -> Result<Self, Op2Error> {
    let mut config: MacroConfig = Self::default();
    for flag in flags {
      if flag == "core" {
        config.core = true;
      } else if flag == "fast" {
        config.fast = true;
      } else {
        return Err(Op2Error::InvalidAttribute(flag.to_string()));
      }
    }
    Ok(config)
  }

  pub fn from_tokens(tokens: TokenStream) -> Result<Self, Op2Error> {
    let attr_string = tokens.to_string();
    let config = std::panic::catch_unwind(|| {
      rules!(tokens => {
        () => {
          Ok(MacroConfig::default())
        }
        ($($flags:ident),+) => {
          Self::from_flags(flags)
        }
      })
    })
    .map_err(|_| Op2Error::PatternMatchFailed("attribute", attr_string))??;
    Ok(config)
  }
}

pub fn op2(
  attr: TokenStream,
  item: TokenStream,
) -> Result<TokenStream, Op2Error> {
  let func = parse2::<ItemFn>(item)?;
  let config = MacroConfig::from_tokens(attr)?;
  generate_op2(config, func)
}

fn generate_op2(
  config: MacroConfig,
  func: ItemFn,
) -> Result<TokenStream, Op2Error> {
  // Create a copy of the original function, named "call"
  let call = Ident::new("call", Span::call_site());
  let mut op_fn = func.clone();
  // Collect non-special attributes
  let attrs = op_fn
    .attrs
    .drain(..)
    .filter(|attr| !is_attribute_special(attr))
    .collect::<Vec<_>>();
  op_fn.sig.generics.params.clear();
  op_fn.sig.ident = call.clone();

  // Clear inert attributes
  // TODO(mmastrac): This should limit itself to clearing ours only
  for arg in op_fn.sig.inputs.iter_mut() {
    match arg {
      FnArg::Receiver(slf) => slf.attrs.clear(),
      FnArg::Typed(ty) => ty.attrs.clear(),
    }
  }

  let signature = parse_signature(func.attrs, func.sig.clone())?;
  if let Some(ident) = signature.lifetime.as_ref().map(|s| format_ident!("{s}"))
  {
    op_fn.sig.generics.params.push(syn2::GenericParam::Lifetime(
      LifetimeParam::new(Lifetime {
        apostrophe: op_fn.span(),
        ident,
      }),
    ));
  }
  let processed_args =
    zip(signature.args.iter(), &func.sig.inputs).collect::<Vec<_>>();

  let mut args = vec![];
  let mut needs_args = false;
  for (index, _) in processed_args.iter().enumerate() {
    let input = format_ident!("arg{index}");
    args.push(input);
    needs_args = true;
  }

  let retval = Ident::new("rv", Span::call_site());
  let result = Ident::new("result", Span::call_site());
  let fn_args = Ident::new("args", Span::call_site());
  let scope = Ident::new("scope", Span::call_site());
  let info = Ident::new("info", Span::call_site());
  let opctx = Ident::new("opctx", Span::call_site());
  let opstate = Ident::new("opstate", Span::call_site());
  let slow_function = Ident::new("v8_fn_ptr", Span::call_site());
  let fast_function = Ident::new("v8_fn_ptr_fast", Span::call_site());
  let fast_api_callback_options =
    Ident::new("fast_api_callback_options", Span::call_site());

  let deno_core = if config.core {
    syn2::parse_str::<Path>("crate")
  } else {
    syn2::parse_str::<Path>("deno_core")
  }
  .expect("Parsing crate should not fail")
  .into_token_stream();

  let mut generator_state = GeneratorState {
    args,
    fn_args,
    call,
    scope,
    info,
    opctx,
    opstate,
    fast_api_callback_options,
    deno_core,
    result,
    retval,
    needs_args,
    slow_function,
    fast_function,
    needs_retval: false,
    needs_scope: false,
    needs_opctx: false,
    needs_opstate: false,
    needs_fast_opctx: false,
    needs_fast_api_callback_options: false,
  };

  let name = func.sig.ident;

  let slow_fn =
    generate_dispatch_slow(&config, &mut generator_state, &signature)?;
  let (fast_definition, fast_fn) =
    match generate_dispatch_fast(&mut generator_state, &signature)? {
      Some((fast_definition, fast_fn)) => {
        if !config.fast {
          return Err(Op2Error::ShouldBeFast);
        }
        (quote!(Some({#fast_definition})), fast_fn)
      }
      None => {
        if config.fast {
          return Err(Op2Error::ShouldNotBeFast);
        }
        (quote!(None), quote!())
      }
    };

  let GeneratorState {
    deno_core,
    slow_function,
    ..
  } = &generator_state;

  let arg_count: usize = generator_state.args.len();
  let vis = func.vis;
  let generic = signature
    .generic_bounds
    .keys()
    .map(|s| format_ident!("{s}"))
    .collect::<Vec<_>>();
  let bound = signature
    .generic_bounds
    .values()
    .map(|p| parse_str::<Path>(p).expect("Failed to reparse path"))
    .collect::<Vec<_>>();

  Ok(quote! {
    #[allow(non_camel_case_types)]
    #(#attrs)*
    #vis struct #name <#(#generic),*> {
      // We need to mark these type parameters as used, so we use a PhantomData
      _unconstructable: ::std::marker::PhantomData<(#(#generic),*)>
    }

    impl <#(#generic : #bound),*> #deno_core::_ops::Op for #name <#(#generic),*> {
      const NAME: &'static str = stringify!(#name);
      const DECL: #deno_core::_ops::OpDecl = #deno_core::_ops::OpDecl::new_internal(
        /*name*/ stringify!(#name),
        /*is_async*/ false,
        /*is_unstable*/ false,
        /*is_v8*/ false,
        /*arg_count*/ #arg_count as u8,
        /*v8_fn_ptr*/ Self::#slow_function as _,
        /*fast_fn*/ #fast_definition,
      );
    }

    impl <#(#generic : #bound),*> #name <#(#generic),*> {
      pub const fn name() -> &'static str {
        stringify!(#name)
      }

      #[deprecated(note = "Use the const op::DECL instead")]
      pub const fn decl() -> #deno_core::_ops::OpDecl {
        <Self as #deno_core::_ops::Op>::DECL
      }

      #fast_fn
      #slow_fn

      #[inline(always)]
      #(#attrs)*
      #op_fn
    }
  })
}

#[cfg(test)]
mod tests {
  use super::*;
  use pretty_assertions::assert_eq;
  use std::path::PathBuf;
  use syn2::parse_str;
  use syn2::File;
  use syn2::Item;
  use syn2::Type;

  #[testing_macros::fixture("op2/test_cases/**/*.rs")]
  fn test_signature_parser(input: PathBuf) {
    let update_expected = std::env::var("UPDATE_EXPECTED").is_ok();

    let source =
      std::fs::read_to_string(&input).expect("Failed to read test file");
    let file = parse_str::<File>(&source).expect("Failed to parse Rust file");
    let mut expected_out = vec![];
    for item in file.items {
      if let Item::Fn(mut func) = item {
        let mut config = None;
        func.attrs.retain(|attr| {
          let tokens = attr.into_token_stream();
          let attr_string = attr.clone().into_token_stream().to_string();
          println!("{}", attr_string);
          use syn2 as syn;
          if let Some(new_config) = rules!(tokens => {
            (#[op2]) => {
              Some(MacroConfig::default())
            }
            (#[op2( $($x:ident),* )]) => {
              Some(MacroConfig::from_flags(x).expect("Failed to parse attribute"))
            }
            (#[$_attr:meta]) => {
              None
            }
          }) {
            config = Some(new_config);
            false
          } else {
            true
          }
        });
        let tokens =
          generate_op2(config.unwrap(), func).expect("Failed to generate op");
        println!("======== Raw tokens ========:\n{}", tokens.clone());
        let tree = syn2::parse2(tokens).unwrap();
        let actual = prettyplease::unparse(&tree);
        println!("======== Generated ========:\n{}", actual);
        expected_out.push(actual);
      }
    }

    let expected_out = expected_out.join("\n");

    if update_expected {
      std::fs::write(input.with_extension("out"), expected_out)
        .expect("Failed to write expectation file");
    } else {
      let expected = std::fs::read_to_string(input.with_extension("out"))
        .expect("Failed to read expectation file");
      assert_eq!(
        expected, expected_out,
        "Failed to match expectation. Use UPDATE_EXPECTED=1."
      );
    }
  }

  #[test]
  fn test_valid_args_md() {
    let update_expected = std::env::var("UPDATE_EXPECTED").is_ok();
    let md = include_str!("valid_args.md");
    let separator = "\n<!-- START -->\n";
    let header = include_str!("README.md").split(separator).next().unwrap();
    let mut actual = format!("{header}{separator}| Rust | Fastcall | v8 |\n|--|--|--|\n");

    // Skip the header and table line
    for line in md.split('\n').skip(2).filter(|s| !s.trim().is_empty() && !s.trim().starts_with('#')) {
      let components = line.split('|').skip(1).map(|s| s.trim()).collect::<Vec<_>>();
      let type_param = components.get(0).unwrap().to_owned();
      let fastcall = components.get(1).unwrap().to_owned();
      let fast = fastcall == "X";
      let v8 = components.get(2).unwrap().to_owned();
      let (attr, ty) = if type_param.starts_with('#') {
        type_param.split_once(' ').expect("Expected an attribute separated by a space (ie: #[attr] type)")
      } else {
        ("", type_param)
      };

      let function = format!("fn op_test({} x: {}) {{}}", attr, ty);
      let function = syn2::parse_str::<ItemFn>(&function).expect("Failed to parse type");
      let sig = parse_signature(vec![], function.sig.clone()).expect("Failed to parse signature");
      generate_op2(MacroConfig { core: false, fast }, function).expect("Failed to generate op");
      actual += &format!("| `{}` | {} | {} |\n", type_param, if fast { "✅" } else { "" }, v8);
    }

    if update_expected {
      std::fs::write("op2/README.md", actual)
        .expect("Failed to write expectation file");
    } else {
      let expected = std::fs::read_to_string("op2/README.md")
        .expect("Failed to read expectation file");
      assert_eq!(
        expected, actual,
        "Failed to match expectation. Use UPDATE_EXPECTED=1."
      );
    }

  }
}
