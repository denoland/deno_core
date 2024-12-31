// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

use super::kw;
use proc_macro2::Ident;
use syn::parse::Parse;
use syn::parse::ParseStream;
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::Error;
use syn::LitStr;
use syn::Token;
use syn::Variant;

pub fn get_variant_name(value: Variant) -> Result<(String, Ident), Error> {
  let mut rename: Option<String> = None;

  if !value.fields.is_empty() {
    return Err(Error::new(
      value.span(),
      "variable with fields are not allowed for enum variants",
    ));
  }

  for attr in value.attrs {
    if attr.path().is_ident("webidl") {
      let list = attr.meta.require_list()?;
      let args = list.parse_args_with(
        Punctuated::<EnumVariantArgument, Token![,]>::parse_terminated,
      )?;

      for argument in args {
        match argument {
          EnumVariantArgument::Rename { value, .. } => {
            rename = Some(value.value())
          }
        }
      }
    }
  }

  Ok((
    rename.unwrap_or_else(|| stringcase::kebab_case(&value.ident.to_string())),
    value.ident,
  ))
}

#[allow(dead_code)]
pub enum EnumVariantArgument {
  Rename {
    name_token: kw::rename,
    eq_token: Token![=],
    value: LitStr,
  },
}

impl Parse for EnumVariantArgument {
  fn parse(input: ParseStream) -> Result<Self, Error> {
    let lookahead = input.lookahead1();
    if lookahead.peek(kw::rename) {
      Ok(EnumVariantArgument::Rename {
        name_token: input.parse()?,
        eq_token: input.parse()?,
        value: input.parse()?,
      })
    } else {
      Err(lookahead.error())
    }
  }
}
