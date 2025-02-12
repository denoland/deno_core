// Copyright 2018-2025 the Deno authors. MIT license.

use proc_macro2::Ident;
use proc_macro2::TokenStream;
use quote::format_ident;
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
  let error_ident = format_ident!("{ident}FromV8Error");

  let out = match input.data {
    Data::Struct(data) => {
      let (fields, pre) = super::structs::get_fields(span, data)?;

      let names = fields
        .iter()
        .map(|field| field.name.clone())
        .collect::<Vec<_>>();

      let error = crate::v8::structs::create_error(
        &error_ident,
        format_ident!("FromV8"),
        &fields,
      );

      let fields = fields.into_iter().map(
        |super::structs::StructField {
           name,
           v8_eternal,
           v8_static,
           error_variant_name,
           ..
         }| {
          quote! {
            let #name = {
              let __key = #v8_eternal
                .with(|__eternal| {
                  if let Some(__key) = __eternal.get(__scope) {
                    Ok(__key)
                  } else {
                    let __key = #v8_static.v8_string(__scope).map_err(#error_ident::FastStringV8Allocation)?;
                    __eternal.set(__scope, __key);
                    Ok(__key)
                  }
                })?
                .cast::<::deno_core::v8::Value>();

              if let Some(__value) = __obj.get(__scope, __key) {
                ::deno_core::convert::FromV8::from_v8(
                  __scope,
                  __value,
                ).map_err(#error_ident::#error_variant_name)?
              } else {
                let __undefined_value = ::deno_core::v8::undefined(__scope).cast::<::deno_core::v8::Value>();

                ::deno_core::convert::FromV8::from_v8(
                  __scope,
                  __undefined_value,
                ).map_err(#error_ident::#error_variant_name)?
              }
            };
          }
        },
      );

      create_from_impl(
        ident,
        &error_ident,
        quote! {
          #pre

          let __obj = __value.try_cast::<::deno_core::v8::Object>().map_err(|_| #error_ident::NotAnObject)?;

          #(#fields)*

          Ok(Self {
            #(#names),*
          })
        },
        error,
      )
    }
    Data::Enum(_) => {
      return Err(Error::new(span, "Enums currently are not supported"))
    }
    Data::Union(_) => return Err(Error::new(span, "Unions are not supported")),
  };

  Ok(out)
}

fn create_from_impl(
  ident: impl ToTokens,
  error_ident: &Ident,
  body: TokenStream,
  extra: TokenStream,
) -> TokenStream {
  quote! {
    #extra

    impl<'a> ::deno_core::convert::FromV8<'a> for #ident {
      type Error = #error_ident;

      fn from_v8(
        __scope: &mut ::deno_core::v8::HandleScope<'a>,
        __value: ::deno_core::v8::Local<'a, deno_core::v8::Value>,
      ) -> Result<Self, Self::Error> {
        #body
      }
    }
  }
}
