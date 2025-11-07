// Copyright 2018-2025 the Deno authors. MIT license.

use crate::conversion::kw as shared_kw;
use crate::conversion::to_v8::convert_or_serde;
use proc_macro2::Ident;
use proc_macro2::Span;
use proc_macro2::TokenStream;
use quote::format_ident;
use quote::quote;
use syn::DataStruct;
use syn::Error;
use syn::Field;
use syn::Fields;
use syn::LitStr;
use syn::Token;
use syn::Type;
use syn::ext::IdentExt;
use syn::parse::Parse;
use syn::parse::ParseStream;
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;

pub fn get_body(span: Span, data: DataStruct) -> Result<TokenStream, Error> {
  match data.fields {
    Fields::Named(fields) => {
      let mut fields = fields
        .named
        .into_iter()
        .map(TryInto::try_into)
        .collect::<Result<Vec<StructField>, Error>>()?;
      fields.sort_by(|a, b| a.name.cmp(&b.name));

      let v8_static_strings = fields
        .iter()
        .map(|field| {
          let name = field.get_js_name();
          let new_ident = format_ident!("__v8_static_{name}");
          let name_str = name.to_string();
          quote!(#new_ident = #name_str)
        })
        .collect::<Vec<_>>();
      let v8_lazy_strings = fields
        .iter()
        .map(|field| {
          let name = field.get_js_name();
          let v8_eternal_name = format_ident!("__v8_{name}_eternal");
          quote! {
            static #v8_eternal_name: ::deno_core::v8::Eternal<::deno_core::v8::String> = ::deno_core::v8::Eternal::empty();
          }
        })
        .collect::<Vec<_>>();

      let fields = fields.into_iter().map(|field| {
        let name = field.get_js_name();
        let original_name = field.name;
        let v8_static_name = format_ident!("__v8_static_{name}");
        let v8_eternal_name = format_ident!("__v8_{name}_eternal");

        let converter = convert_or_serde(field.serde, field.ty.span(), quote!(self.#original_name));

        quote! {
          {
            let __key = #v8_eternal_name
              .with(|__eternal| {
                if let Some(__key) = __eternal.get(__scope) {
                  Ok(__key)
                } else {
                  let __key = #v8_static_name.v8_string(__scope)?;
                  __eternal.set(__scope, __key);
                  Ok(__key)
                }
              }).map_err(::deno_error::JsErrorBox::from_err::<::deno_core::FastStringV8AllocationError>)?
              .into();

            let __value = #converter;

            __obj.set(__scope, __key, __value);
          };
        }
      }).collect::<Vec<_>>();

      let body = quote! {
        ::deno_core::v8_static_strings! {
          #(#v8_static_strings),*
        }

        thread_local! {
          #(#v8_lazy_strings)*
        }

        let __obj = ::deno_core::v8::Object::new(__scope);

        #(#fields)*

        Ok(__obj.into())
      };

      Ok(body)
    }
    Fields::Unnamed(fields) => {
      let fields = fields
        .unnamed
        .into_iter()
        .enumerate()
        .map(TryInto::try_into)
        .collect::<Result<Vec<StructTupleField>, Error>>()?;

      let value = if fields.len() == 1 {
        let field = fields.first().unwrap();
        let converter =
          convert_or_serde(field.serde, field.span, quote!(self.0));
        quote!(Ok(#converter))
      } else {
        let fields = fields
          .into_iter()
          .map(|field| {
            let i = syn::Index::from(field.i);
            convert_or_serde(field.serde, field.span, quote!(self.#i))
          })
          .collect::<Vec<_>>();

        quote! {
          let __value = &[#(#fields),*];
          Ok(v8::Array::new_with_elements(__scope, __value).into())
        }
      };

      Ok(value)
    }
    Fields::Unit => {
      Err(Error::new(span, "Unit fields are currently not supported"))
    }
  }
}

struct StructField {
  span: Span,
  name: Ident,
  rename: Option<String>,
  serde: bool,
  ty: Type,
}

impl StructField {
  fn get_js_name(&self) -> Ident {
    Ident::new(
      &self
        .rename
        .clone()
        .unwrap_or_else(|| stringcase::camel_case(&self.name.to_string())),
      self.span,
    )
  }
}

impl TryFrom<Field> for StructField {
  type Error = Error;
  fn try_from(value: Field) -> Result<Self, Self::Error> {
    let span = value.span();
    let crate::conversion::SharedAttribute {
      mut rename,
      mut serde,
    } = crate::conversion::StructFieldArgumentShared::parse(&value.attrs)?;

    for attr in value.attrs {
      if attr.path().is_ident("to_v8") {
        let list = attr.meta.require_list()?;
        let args = list.parse_args_with(
          Punctuated::<StructFieldArgument, Token![,]>::parse_terminated,
        )?;

        for argument in args {
          match argument {
            StructFieldArgument::Rename { value, .. } => {
              rename = Some(value.value())
            }
            StructFieldArgument::Serde { .. } => serde = true,
          }
        }
      }
    }

    let name = value.ident.unwrap();
    if rename.is_none() {
      rename = Some(stringcase::camel_case(&name.unraw().to_string()));
    }

    Ok(Self {
      span,
      name,
      rename,
      serde,
      ty: value.ty,
    })
  }
}

#[allow(dead_code)]
enum StructFieldArgument {
  Rename {
    name_token: shared_kw::rename,
    eq_token: Token![=],
    value: LitStr,
  },
  Serde {
    name_token: shared_kw::serde,
  },
}

impl Parse for StructFieldArgument {
  fn parse(input: ParseStream) -> Result<Self, Error> {
    let lookahead = input.lookahead1();
    if lookahead.peek(shared_kw::rename) {
      Ok(StructFieldArgument::Rename {
        name_token: input.parse()?,
        eq_token: input.parse()?,
        value: input.parse()?,
      })
    } else if lookahead.peek(shared_kw::serde) {
      Ok(StructFieldArgument::Serde {
        name_token: input.parse()?,
      })
    } else {
      Err(lookahead.error())
    }
  }
}

struct StructTupleField {
  span: Span,
  i: usize,
  serde: bool,
}

impl TryFrom<(usize, Field)> for StructTupleField {
  type Error = Error;
  fn try_from((i, value): (usize, Field)) -> Result<Self, Self::Error> {
    let span = value.span();
    let crate::conversion::SharedTupleAttribute { mut serde } =
      crate::conversion::StructTupleFieldArgumentShared::parse(&value.attrs)?;

    for attr in value.attrs {
      if attr.path().is_ident("to_v8") {
        let list = attr.meta.require_list()?;
        let args = list.parse_args_with(
          Punctuated::<StructTupleFieldArgument, Token![,]>::parse_terminated,
        )?;

        for argument in args {
          match argument {
            StructTupleFieldArgument::Serde { .. } => serde = true,
          }
        }
      }
    }

    Ok(Self { span, i, serde })
  }
}

#[allow(dead_code)]
enum StructTupleFieldArgument {
  Serde {
    name_token: crate::conversion::kw::serde,
  },
}

impl Parse for StructTupleFieldArgument {
  fn parse(input: ParseStream) -> Result<Self, Error> {
    let lookahead = input.lookahead1();
    if lookahead.peek(shared_kw::serde) {
      Ok(StructTupleFieldArgument::Serde {
        name_token: input.parse()?,
      })
    } else {
      Err(lookahead.error())
    }
  }
}
