// Copyright 2018-2025 the Deno authors. MIT license.

use proc_macro2::Ident;
use proc_macro2::Span;
use proc_macro2::TokenStream;
use quote::format_ident;
use quote::quote;
use syn::ext::IdentExt;
use syn::parse::Parse;
use syn::parse::ParseStream;
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::DataStruct;
use syn::Error;
use syn::Field;
use syn::Fields;
use syn::LitStr;
use syn::Token;

pub fn get_fields(
  span: Span,
  data: DataStruct,
) -> Result<(Vec<StructField>, TokenStream), Error> {
  let fields = match data.fields {
    Fields::Named(fields) => fields,
    Fields::Unnamed(_) => {
      return Err(Error::new(
        span,
        "Unnamed fields are currently not supported",
      ))
    }
    Fields::Unit => {
      return Err(Error::new(span, "Unit fields are currently not supported"))
    }
  };

  let mut fields = fields
    .named
    .into_iter()
    .map(TryInto::try_into)
    .collect::<Result<Vec<StructField>, Error>>()?;
  fields.sort_by(|a, b| a.name.cmp(&b.name));

  let v8_static_strings = fields
    .iter()
    .map(|field| {
      let static_name = &field.v8_static;
      let name_str = static_name.to_string();
      quote!(#static_name = #name_str)
    })
    .collect::<Vec<_>>();
  let v8_lazy_strings = fields
    .iter()
    .map(|field| {
      let name = &field.v8_eternal;
      quote! {
        static #name: ::deno_core::v8::Eternal<::deno_core::v8::String> = ::deno_core::v8::Eternal::empty();
      }
    })
    .collect::<Vec<_>>();

  let pre = quote! {
    ::deno_core::v8_static_strings! {
      #(#v8_static_strings),*
    }

    thread_local! {
      #(#v8_lazy_strings)*
    }
  };

  Ok((fields, pre))
}

pub struct StructField {
  pub name: Ident,
  pub v8_static: Ident,
  pub v8_eternal: Ident,
  pub get_key: TokenStream,
}

impl TryFrom<Field> for StructField {
  type Error = Error;
  fn try_from(value: Field) -> Result<Self, Self::Error> {
    let span = value.span();
    let name = value.ident.unwrap();
    let mut rename = stringcase::camel_case(&name.unraw().to_string());

    for attr in value.attrs {
      if attr.path().is_ident("v8") {
        let list = attr.meta.require_list()?;
        let args = list.parse_args_with(
          Punctuated::<StructFieldArgument, Token![,]>::parse_terminated,
        )?;

        for argument in args {
          match argument {
            StructFieldArgument::Rename { value, .. } => {
              rename = value.value();
            }
          }
        }
      }
    }

    let js_name = Ident::new(&rename, span);

    let v8_static = format_ident!("__v8_static_{js_name}");
    let v8_eternal = format_ident!("__v8_{js_name}_eternal");

    let get_key = quote! {
      #v8_eternal
        .with(|__eternal| {
          if let Some(__key) = __eternal.get(__scope) {
            Ok(__key)
          } else {
            let __key = #v8_static.v8_string(__scope).map_err(::deno_error::JsErrorBox::from_err)?;
            __eternal.set(__scope, __key);
            Ok::<_, ::deno_error::JsErrorBox>(__key)
          }
        })?
        .cast::<::deno_core::v8::Value>()
    };

    Ok(Self {
      v8_static,
      v8_eternal,
      get_key,
      name,
    })
  }
}

mod kw {
  syn::custom_keyword!(rename);
}

#[allow(dead_code)]
enum StructFieldArgument {
  Rename {
    name_token: kw::rename,
    eq_token: Token![=],
    value: LitStr,
  },
}

impl Parse for StructFieldArgument {
  fn parse(input: ParseStream) -> Result<Self, Error> {
    let lookahead = input.lookahead1();
    if lookahead.peek(kw::rename) {
      Ok(StructFieldArgument::Rename {
        name_token: input.parse()?,
        eq_token: input.parse()?,
        value: input.parse()?,
      })
    } else {
      Err(lookahead.error())
    }
  }
}
