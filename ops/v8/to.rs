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

pub fn to_v8(item: TokenStream) -> Result<TokenStream, Error> {
  let input = parse2::<DeriveInput>(item)?;
  let span = input.span();
  let ident = input.ident;
  let error_ident = format_ident!("{ident}ToV8Error");

  let out = match input.data {
    Data::Struct(data) => {
      let (fields, pre) = super::structs::get_fields(span, data)?;

      let error = crate::v8::structs::create_error(
        &error_ident,
        format_ident!("ToV8"),
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
            {
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

              let __value = ::deno_core::convert::ToV8::to_v8(
                self.#name,
                __scope,
              ).map_err(#error_ident::#error_variant_name)?;

              __obj.set(__scope, __key, __value);
            }
          }
        },
      );

      create_from_impl(
        ident,
        &error_ident,
        quote! {
          #pre

          let __obj = ::deno_core::v8::Object::new(__scope);

          #(#fields)*

          Ok(__obj.into())
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

    impl<'a> ::deno_core::convert::ToV8<'a> for #ident {
      type Error = #error_ident;

      fn to_v8(
        self,
        __scope: &mut ::deno_core::v8::HandleScope<'a>,
      ) -> Result<::deno_core::v8::Local<'a, ::deno_core::v8::Value>, Self::Error> {
        #body
      }
    }
  }
}
