// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

mod dictionary;
mod r#enum;

use proc_macro2::Ident;
use proc_macro2::TokenStream;
use quote::format_ident;
use quote::quote;
use quote::ToTokens;
use std::collections::HashMap;
use syn::parse::Parse;
use syn::parse::ParseStream;
use syn::parse2;
use syn::spanned::Spanned;
use syn::Attribute;
use syn::Data;
use syn::DeriveInput;
use syn::Error;
use syn::Fields;
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
        let fields = match data.fields {
          Fields::Named(fields) => fields,
          Fields::Unnamed(_) => {
            return Err(Error::new(
              span,
              "Unnamed fields are currently not supported",
            ))
          }
          Fields::Unit => {
            return Err(Error::new(
              span,
              "Unit fields are currently not supported",
            ))
          }
        };

        let mut fields = fields
          .named
          .into_iter()
          .map(TryInto::try_into)
          .collect::<Result<Vec<dictionary::DictionaryField>, Error>>(
        )?;
        fields.sort_by(|a, b| a.name.cmp(&b.name));

        let names = fields
          .iter()
          .map(|field| field.name.clone())
          .collect::<Vec<_>>();
        let v8_static_strings = fields
          .iter()
          .map(|field| {
            let name = field.get_name();
            let new_ident = format_ident!("__v8_static_{name}");
            let name_str = name.to_string();
            quote!(#new_ident = #name_str)
          })
          .collect::<Vec<_>>();
        let v8_lazy_strings = fields
          .iter()
          .map(|field| {
            let name = field.get_name();
            let v8_eternal_name = format_ident!("__v8_{name}_eternal");
            quote! {
              static #v8_eternal_name: ::deno_core::v8::Eternal<::deno_core::v8::String> = ::deno_core::v8::Eternal::empty();
            }
          })
          .collect::<Vec<_>>();

        let fields = fields.into_iter().map(|field| {
          let name = field.get_name();
          let string_name = name.to_string();
          let original_name = field.name;
          let v8_static_name = format_ident!("__v8_static_{name}");
          let v8_eternal_name = format_ident!("__v8_{name}_eternal");

          let required_or_default = if field.required && field.default_value.is_none() {
            quote! {
              return Err(::deno_core::webidl::WebIdlError::new(
                __prefix,
                &__context,
                ::deno_core::webidl::WebIdlErrorKind::DictionaryCannotConvertKey {
                  converter: #ident_string,
                  key: #string_name,
                },
              ));
            }
          } else if let Some(default) = field.default_value {
            default.to_token_stream()
          } else {
            quote! { None }
          };

          let val = if field.required {
            quote!(val)
          } else {
            quote!(Some(val))
          };

          let options = if field.converter_options.is_empty() {
            quote!(Default::default())
          } else {
            let inner = field.converter_options
              .into_iter()
              .map(|(k, v)| quote!(#k: #v))
              .collect::<Vec<_>>();

            let ty = field.ty;

            // Type-alias to workaround https://github.com/rust-lang/rust/issues/86935
            quote! {
              {
                type Alias<'a> = <#ty as ::deno_core::webidl::WebIdlConverter<'a>>::Options;
                Alias {
                  #(#inner),*,
                  ..Default::default()
                }
              }
            }
          };

          let new_context = format!("'{string_name}' of '{ident_string}'");

          quote! {
            let #original_name = {
              let __key = #v8_eternal_name
                .with(|__eternal| {
                  if let Some(__key) = __eternal.get(__scope) {
                    Ok(__key)
                  } else {
                    let __key = #v8_static_name
                      .v8_string(__scope)
                      .map_err(|e| ::deno_core::webidl::WebIdlError::other(__prefix.clone(), &__context, e))?;
                    __eternal.set(__scope, __key);
                    Ok(__key)
                  }
                })?
                .into();

              if let Some(__value) = __obj.as_ref()
              .and_then(|__obj| __obj.get(__scope, __key))
              .and_then(|__value| {
                if __value.is_undefined() {
                  None
                } else {
                  Some(__value)
                }
              }) {
                let val = ::deno_core::webidl::WebIdlConverter::convert(
                  __scope,
                  __value,
                  __prefix.clone(),
                  || format!("{} ({})", #new_context, __context()).into(),
                  &#options,
                )?;
                #val
              } else {
                #required_or_default
              }
            };
          }
        }).collect::<Vec<_>>();

        let implementation = create_impl(
          ident,
          quote! {
            let __obj: Option<::deno_core::v8::Local<::deno_core::v8::Object>> = if __value.is_undefined() || __value.is_null() {
              None
            } else {
              if let Ok(obj) = __value.try_into() {
                Some(obj)
              } else {
                return Err(::deno_core::webidl::WebIdlError::new(
                  __prefix,
                  &__context,
                  ::deno_core::webidl::WebIdlErrorKind::ConvertToConverterType("dictionary")
                ));
              }
            };

            #(#fields)*

            Ok(Self { #(#names),* })
          },
        );

        quote! {
          ::deno_core::v8_static_strings! {
            #(#v8_static_strings),*
          }

          thread_local! {
            #(#v8_lazy_strings)*
          }

          #implementation
        }
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
        let variants = data
          .variants
          .into_iter()
          .map(r#enum::get_variant_name)
          .collect::<Result<HashMap<_, _>, _>>()?;

        let variants = variants
          .into_iter()
          .map(|(name, ident)| quote!(#name => Ok(Self::#ident)))
          .collect::<Vec<_>>();

        create_impl(
          ident,
          quote! {
            let Ok(str) = __value.try_cast::<::deno_core::v8::String>() else {
              return Err(::deno_core::webidl::WebIdlError::new(
                __prefix,
                &__context,
                ::deno_core::webidl::WebIdlErrorKind::ConvertToConverterType("enum"),
              ));
            };

            match str.to_rust_string_lossy(__scope).as_str() {
              #(#variants),*,
              s => Err(::deno_core::webidl::WebIdlError::new(__prefix, &__context, ::deno_core::webidl::WebIdlErrorKind::InvalidEnumVariant { converter: #ident_string, variant: s.to_string() }))
            }
          },
        )
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

    const PRELUDE: &str = r"// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
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
