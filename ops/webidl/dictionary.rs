// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

use super::kw;
use proc_macro2::Ident;
use proc_macro2::Span;
use syn::parse::Parse;
use syn::parse::ParseStream;
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::Error;
use syn::Expr;
use syn::Field;
use syn::LitStr;
use syn::MetaNameValue;
use syn::Token;
use syn::Type;

pub struct DictionaryField {
  pub span: Span,
  pub name: Ident,
  pub rename: Option<String>,
  pub default_value: Option<Expr>,
  pub required: bool,
  pub converter_options: std::collections::HashMap<Ident, Expr>,
  pub ty: Type,
}

impl DictionaryField {
  pub fn get_name(&self) -> Ident {
    Ident::new(
      &self
        .rename
        .clone()
        .unwrap_or_else(|| stringcase::camel_case(&self.name.to_string())),
      self.span,
    )
  }
}

impl TryFrom<Field> for DictionaryField {
  type Error = Error;
  fn try_from(value: Field) -> Result<Self, Self::Error> {
    let span = value.span();
    let mut default_value: Option<Expr> = None;
    let mut rename: Option<String> = None;
    let mut required = false;
    let mut converter_options = std::collections::HashMap::new();

    for attr in value.attrs {
      if attr.path().is_ident("webidl") {
        let list = attr.meta.require_list()?;
        let args = list.parse_args_with(
          Punctuated::<DictionaryFieldArgument, Token![,]>::parse_terminated,
        )?;

        for argument in args {
          match argument {
            DictionaryFieldArgument::Default { value, .. } => {
              default_value = Some(value)
            }
            DictionaryFieldArgument::Rename { value, .. } => {
              rename = Some(value.value())
            }
            DictionaryFieldArgument::Required { .. } => required = true,
          }
        }
      } else if attr.path().is_ident("options") {
        let list = attr.meta.require_list()?;
        let args = list.parse_args_with(
          Punctuated::<MetaNameValue, Token![,]>::parse_terminated,
        )?;

        let args = args
          .into_iter()
          .map(|kv| {
            let ident = kv.path.require_ident()?;
            Ok((ident.clone(), kv.value))
          })
          .collect::<Result<Vec<_>, Error>>()?;

        converter_options.extend(args);
      }
    }

    let is_option = if let Type::Path(path) = &value.ty {
      if let Some(last) = path.path.segments.last() {
        last.ident == "Option"
      } else {
        false
      }
    } else {
      false
    };

    Ok(Self {
      span,
      name: value.ident.unwrap(),
      rename,
      default_value,
      required: required || !is_option,
      converter_options,
      ty: value.ty,
    })
  }
}

#[allow(dead_code)]
pub enum DictionaryFieldArgument {
  Default {
    name_token: kw::default,
    eq_token: Token![=],
    value: Expr,
  },
  Rename {
    name_token: kw::rename,
    eq_token: Token![=],
    value: LitStr,
  },
  Required {
    name_token: kw::rename,
  },
}

impl Parse for DictionaryFieldArgument {
  fn parse(input: ParseStream) -> Result<Self, Error> {
    let lookahead = input.lookahead1();
    if lookahead.peek(kw::default) {
      Ok(DictionaryFieldArgument::Default {
        name_token: input.parse()?,
        eq_token: input.parse()?,
        value: input.parse()?,
      })
    } else if lookahead.peek(kw::rename) {
      Ok(DictionaryFieldArgument::Rename {
        name_token: input.parse()?,
        eq_token: input.parse()?,
        value: input.parse()?,
      })
    } else if lookahead.peek(kw::required) {
      Ok(DictionaryFieldArgument::Required {
        name_token: input.parse()?,
      })
    } else {
      Err(lookahead.error())
    }
  }
}
