// Copyright 2018-2025 the Deno authors. MIT license.

pub mod from;
mod structs;
pub mod to;

#[cfg(test)]
mod tests {
  use proc_macro2::Ident;
  use proc_macro2::TokenStream;
  use quote::ToTokens;
  use std::path::PathBuf;
  use syn::Item;
  use syn::punctuated::Punctuated;

  fn derives_v8<'a>(
    attrs: impl IntoIterator<Item = &'a syn::Attribute>,
  ) -> bool {
    attrs.into_iter().any(|attr| {
      attr.path().is_ident("derive") && {
        let list = attr.meta.require_list().unwrap();
        let idents = list
          .parse_args_with(
            Punctuated::<Ident, syn::Token![,]>::parse_terminated,
          )
          .unwrap();
        idents
          .iter()
          .any(|ident| matches!(ident.to_string().as_str(), "FromV8" | "ToV8"))
      }
    })
  }

  fn expand_v8(item: impl ToTokens) -> TokenStream {
    let mut tokens = super::from::from_v8(item.to_token_stream())
      .expect("Failed to generate FromV8");

    tokens.extend(
      super::to::to_v8(item.to_token_stream())
        .expect("Failed to generate ToV8"),
    );

    tokens
  }

  #[testing_macros::fixture("v8/test_cases/*.rs")]
  fn test_proc_macro_sync(input: PathBuf) {
    crate::infra::run_macro_expansion_test(input, |file| {
      file.items.into_iter().filter_map(|item| {
        if let Item::Struct(struct_item) = item
          && derives_v8(&struct_item.attrs)
        {
          return Some(expand_v8(struct_item));
        }

        None
      })
    })
  }
}
