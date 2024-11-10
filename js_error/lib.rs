// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#![deny(clippy::unnecessary_wraps)]

mod js_error;

use proc_macro::TokenStream;

#[proc_macro_derive(JsError, attributes(class, property, inherit))]
pub fn derive_js_error(item: TokenStream) -> TokenStream {
  js_error_macro(item)
}

fn js_error_macro(item: TokenStream) -> TokenStream {
  match js_error::js_error(item.into()) {
    Ok(output) => output.into(),
    Err(err) => err.into_compile_error().into(),
  }
}
