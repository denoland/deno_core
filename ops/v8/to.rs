// Copyright 2018-2025 the Deno authors. MIT license.

use proc_macro2::TokenStream;
use quote::ToTokens;
use quote::quote;
use syn::Data;
use syn::DeriveInput;
use syn::Error;
use syn::parse2;
use syn::spanned::Spanned;

pub fn to_v8(item: TokenStream) -> Result<TokenStream, Error> {
  let input = parse2::<DeriveInput>(item)?;
  let span = input.span();
  let ident = input.ident;

  let out = match input.data {
    Data::Struct(data) => {
      let (fields, pre) = super::structs::get_fields(span, data)?;

      let fields = fields.into_iter().map(
        |super::structs::StructField { name, get_key, .. }| {
          quote! {
            {
              let __key = #get_key;

              let __value = ::deno_core::convert::ToV8::to_v8(
                self.#name,
                __scope,
              ).map_err(::deno_error::JsErrorBox::from_err)?;

              __obj.set(__scope, __key, __value);
            }
          }
        },
      );

      create_from_impl(
        ident,
        quote! {
          #pre

          let __obj = ::deno_core::v8::Object::new(__scope);

          #(#fields)*

          Ok(__obj.into())
        },
      )
    }
    Data::Enum(_) => {
      return Err(Error::new(span, "Enums currently are not supported"));
    }
    Data::Union(_) => return Err(Error::new(span, "Unions are not supported")),
  };

  Ok(out)
}

fn create_from_impl(ident: impl ToTokens, body: TokenStream) -> TokenStream {
  quote! {
    impl<'a> ::deno_core::convert::ToV8<'a> for #ident {
      type Error = ::deno_error::JsErrorBox;

      fn to_v8(
        self,
        __scope: &mut ::deno_core::v8::HandleScope<'a>,
      ) -> Result<::deno_core::v8::Local<'a, ::deno_core::v8::Value>, Self::Error> {
        #body
      }
    }
  }
}
