// Copyright 2018-2025 the Deno authors. MIT license.

mod dictionary;
mod r#enum;

use proc_macro2::Ident;
use proc_macro2::TokenStream;
use quote::quote;
use syn::parse::Parse;
use syn::parse::ParseStream;
use syn::parse2;
use syn::spanned::Spanned;
use syn::Attribute;
use syn::Data;
use syn::DeriveInput;
use syn::Error;
use syn::Token;

pub fn webidl(item: TokenStream) -> Result<TokenStream, Error> {
  let input = parse2::<DeriveInput>(item)?;
  let span = input.span();
  let ident = input.ident;
  let ident_string = ident.to_string();
  let converter = input
    .attrs
    .into_iter()
    .find_map(|attr| ConverterType::from_attribute(attr).transpose())
    .ok_or_else(|| Error::new(span, "missing #[webidl] attribute"))??;

  let out = match input.data {
    Data::Struct(data) => match converter {
      ConverterType::Dictionary => {
        create_impl(ident, dictionary::get_body(ident_string, span, data)?)
      }
      ConverterType::Enum => {
        return Err(Error::new(span, "Structs do not support enum converters"));
      }
    },
    Data::Enum(data) => match converter {
      ConverterType::Dictionary => {
        return Err(Error::new(
          span,
          "Enums currently do not support dictionary converters",
        ));
      }
      ConverterType::Enum => {
        create_impl(ident, r#enum::get_body(ident_string, data)?)
      }
    },
    Data::Union(_) => return Err(Error::new(span, "Unions are not supported")),
  };

  Ok(out)
}

mod kw {
  syn::custom_keyword!(dictionary);
  syn::custom_keyword!(default);
  syn::custom_keyword!(rename);
  syn::custom_keyword!(required);
}

enum ConverterType {
  Dictionary,
  Enum,
}

impl ConverterType {
  fn from_attribute(attr: Attribute) -> Result<Option<Self>, Error> {
    if attr.path().is_ident("webidl") {
      let list = attr.meta.require_list()?;
      let value = list.parse_args::<Self>()?;
      Ok(Some(value))
    } else {
      Ok(None)
    }
  }
}

impl Parse for ConverterType {
  fn parse(input: ParseStream) -> syn::Result<Self> {
    let lookahead = input.lookahead1();

    if lookahead.peek(kw::dictionary) {
      input.parse::<kw::dictionary>()?;
      Ok(Self::Dictionary)
    } else if lookahead.peek(Token![enum]) {
      input.parse::<Token![enum]>()?;
      Ok(Self::Enum)
    } else {
      Err(lookahead.error())
    }
  }
}

fn create_impl(ident: Ident, body: TokenStream) -> TokenStream {
  quote! {
    impl<'a> ::deno_core::webidl::WebIdlConverter<'a> for #ident {
      type Options = ();

      fn convert<C>(
        __scope: &mut ::deno_core::v8::HandleScope<'a>,
        __value: ::deno_core::v8::Local<'a, ::deno_core::v8::Value>,
        __prefix: std::borrow::Cow<'static, str>,
        __context: C,
        __options: &Self::Options,
      ) -> Result<Self, ::deno_core::webidl::WebIdlError>
      where
        C: Fn() -> std::borrow::Cow<'static, str>,
      {
        #body
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use pretty_assertions::assert_eq;
  use quote::ToTokens;
  use std::path::PathBuf;
  use syn::punctuated::Punctuated;
  use syn::File;
  use syn::Item;

  fn derives_webidl<'a>(
    attrs: impl IntoIterator<Item = &'a Attribute>,
  ) -> bool {
    attrs.into_iter().any(|attr| {
      attr.path().is_ident("derive") && {
        let list = attr.meta.require_list().unwrap();
        let idents = list
          .parse_args_with(Punctuated::<Ident, Token![,]>::parse_terminated)
          .unwrap();
        idents.iter().any(|ident| ident == "WebIDL")
      }
    })
  }

  #[testing_macros::fixture("webidl/test_cases/*.rs")]
  fn test_proc_macro_sync(input: PathBuf) {
    test_proc_macro_output(input)
  }

  fn expand_webidl(item: impl ToTokens) -> String {
    let tokens =
      webidl(item.to_token_stream()).expect("Failed to generate WebIDL");
    println!("======== Raw tokens ========:\n{}", tokens.clone());
    let tree = syn::parse2(tokens).unwrap();
    let actual = prettyplease::unparse(&tree);
    println!("======== Generated ========:\n{}", actual);
    actual
  }

  fn test_proc_macro_output(input: PathBuf) {
    let update_expected = std::env::var("UPDATE_EXPECTED").is_ok();

    let source =
      std::fs::read_to_string(&input).expect("Failed to read test file");

    const PRELUDE: &str = r"// Copyright 2018-2025 the Deno authors. MIT license.

#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();";

    if !source.starts_with(PRELUDE) {
      panic!("Source does not start with expected prelude:]n{PRELUDE}");
    }

    let file =
      syn::parse_str::<File>(&source).expect("Failed to parse Rust file");
    let mut expected_out = vec![];
    for item in file.items {
      match item {
        Item::Struct(struct_item) => {
          if derives_webidl(&struct_item.attrs) {
            expected_out.push(expand_webidl(struct_item));
          }
        }
        Item::Enum(enum_item) => {
          dbg!();
          if derives_webidl(&enum_item.attrs) {
            expected_out.push(expand_webidl(enum_item));
          }
        }
        _ => {}
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
}
