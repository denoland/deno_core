// Copyright 2018-2025 the Deno authors. MIT license.

use proc_macro2::TokenStream;
use quote::quote;
use quote::ToTokens;
use syn::parse2;
use syn::spanned::Spanned;
use syn::Data;
use syn::DeriveInput;
use syn::Error;

pub fn from_v8(item: TokenStream) -> Result<TokenStream, Error> {
  let input = parse2::<DeriveInput>(item)?;
  let span = input.span();
  let ident = input.ident;

  let out = match input.data {
    Data::Struct(data) => {
      let (fields, pre) = super::structs::get_fields(span, data)?;

      let names = fields
        .iter()
        .map(|field| field.name.clone())
        .collect::<Vec<_>>();

      let fields = fields.into_iter().map(
        |super::structs::StructField {
           name,
           get_key,
           ..
         }| {
          quote! {
            let #name = {
              let __key = #get_key;

              if let Some(__value) = __obj.get(__scope, __key) {
                ::deno_core::convert::FromV8::from_v8(
                  __scope,
                  __value,
                ).map_err(::deno_error::JsErrorBox::from_err)?
              } else {
                let __undefined_value = ::deno_core::v8::undefined(__scope).cast::<::deno_core::v8::Value>();

                ::deno_core::convert::FromV8::from_v8(
                  __scope,
                  __undefined_value,
                ).map_err(::deno_error::JsErrorBox::from_err)?
              }
            };
          }
        },
      );

      create_from_impl(
        ident,
        quote! {
          #pre

          let __obj = __value.try_cast::<::deno_core::v8::Object>()
            .map_err(|_| ::deno_error::JsErrorBox::type_error("Not an object"))?;

          #(#fields)*

          Ok(Self {
            #(#names),*
          })
        },
      )
    }
    Data::Enum(_) => {
      return Err(Error::new(span, "Enums currently are not supported"))
    }
    Data::Union(_) => return Err(Error::new(span, "Unions are not supported")),
  };

  Ok(out)
}

fn create_from_impl(ident: impl ToTokens, body: TokenStream) -> TokenStream {
  quote! {
    impl<'a> ::deno_core::convert::FromV8<'a> for #ident {
      type Error = ::deno_error::JsErrorBox;

      fn from_v8(
        __scope: &mut ::deno_core::v8::HandleScope<'a>,
        __value: ::deno_core::v8::Local<'a, deno_core::v8::Value>,
      ) -> Result<Self, Self::Error> {
        #body
      }
    }
  }
}
