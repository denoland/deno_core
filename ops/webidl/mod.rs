// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

use proc_macro2::Ident;
use proc_macro2::TokenStream;
use quote::format_ident;
use quote::quote;
use quote::ToTokens;
use syn::parse::Parse;
use syn::parse::ParseStream;
use syn::parse2;
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::Attribute;
use syn::Data;
use syn::DeriveInput;
use syn::Error;
use syn::Expr;
use syn::Field;
use syn::Fields;
use syn::LitStr;
use syn::MetaNameValue;
use syn::Token;
use syn::Type;

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
          .collect::<Result<Vec<DictionaryField>, Error>>()?;
        fields.sort_by(|a, b| a.name.cmp(&b.name));

        let names = fields
          .iter()
          .map(|field| field.name.clone())
          .collect::<Vec<_>>();
        let v8_static_strings = fields
          .iter()
          .map(|field| {
            let name = &field.name;
            let new_ident = format_ident!("__v8_static_{name}");
            let value = field
              .rename
              .clone()
              .unwrap_or_else(|| stringcase::camel_case(&name.to_string()));
            quote!(#new_ident = #value)
          })
          .collect::<Vec<_>>();

        let fields = fields.into_iter().map(|field| {
          let string_name = field.name.to_string();
          let name = field.name;
          let v8_static_name = format_ident!("__v8_static_{name}");

          let required_or_default = if field.required && field.default_value.is_none() {
            quote! {
              return Err(::deno_core::webidl::WebIdlError::new(
                __prefix,
                __context,
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

          println!("{:?}", options.to_string());

          quote! {
            let #name = {
              let __key = #v8_static_name
                .v8_string(__scope)
                .map_err(|e| ::deno_core::webidl::WebIdlError::other(__prefix.clone(), __context.clone(), e))?
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
                  format!("'{}' of '{}' ({__context})", #string_name, #ident_string).into(),
                  &#options,
                )?;
                #val
              } else {
                #required_or_default
              }
            };
          }
        }).collect::<Vec<_>>();

        quote! {
          ::deno_core::v8_static_strings! {
            #(#v8_static_strings),*
          }

          impl<'a> ::deno_core::webidl::WebIdlConverter<'a> for #ident {
            type Options = ();

            fn convert(
              __scope: &mut::deno_core:: v8::HandleScope<'a>,
              __value: ::deno_core::v8::Local<'a, ::deno_core::v8::Value>,
              __prefix: std::borrow::Cow<'static, str>,
              __context: std::borrow::Cow<'static, str>,
              __options: &Self::Options,
            ) -> Result<Self, ::deno_core::webidl::WebIdlError> {
              let __obj = if __value.is_undefined() || __value.is_null() {
                None
              } else {
                Some(__value.to_object(__scope).ok_or_else(|| {
                  ::deno_core::webidl::WebIdlError::new(
                    __prefix.clone(),
                    __context.clone(),
                    ::deno_core::webidl::WebIdlErrorKind::ConvertToConverterType("dictionary")
                  )
                })?)
              };

              #(#fields)*

              Ok(Self { #(#names),* })
            }
          }
        }
      }
    },
    Data::Enum(_) => {
      return Err(Error::new(span, "Enums are currently not supported"));
    }
    Data::Union(_) => {
      return Err(Error::new(span, "Unions are currently not supported"))
    }
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
    } else {
      Err(lookahead.error())
    }
  }
}

struct DictionaryField {
  name: Ident,
  rename: Option<String>,
  default_value: Option<Expr>,
  required: bool,
  converter_options: std::collections::HashMap<Ident, Expr>,
  ty: Type,
}

impl TryFrom<Field> for DictionaryField {
  type Error = Error;
  fn try_from(value: Field) -> Result<Self, Self::Error> {
    let mut default_value: Option<Expr> = None;
    let mut rename: Option<String> = None;
    let mut required = false;
    let mut converter_options = std::collections::HashMap::new();

    for attr in value.attrs {
      if attr.path().is_ident("webidl") {
        let list = attr.meta.require_list()?;
        let args = list.parse_args_with(
          Punctuated::<FieldArgument, Token![,]>::parse_terminated,
        )?;

        for argument in args {
          match argument {
            FieldArgument::Default { value, .. } => default_value = Some(value),
            FieldArgument::Rename { value, .. } => rename = Some(value.value()),
            FieldArgument::Required { .. } => required = true,
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
enum FieldArgument {
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

impl Parse for FieldArgument {
  fn parse(input: ParseStream) -> Result<Self, Error> {
    let lookahead = input.lookahead1();
    if lookahead.peek(kw::default) {
      Ok(FieldArgument::Default {
        name_token: input.parse()?,
        eq_token: input.parse()?,
        value: input.parse()?,
      })
    } else if lookahead.peek(kw::rename) {
      Ok(FieldArgument::Rename {
        name_token: input.parse()?,
        eq_token: input.parse()?,
        value: input.parse()?,
      })
    } else if lookahead.peek(kw::required) {
      Ok(FieldArgument::Required {
        name_token: input.parse()?,
      })
    } else {
      Err(lookahead.error())
    }
  }
}
