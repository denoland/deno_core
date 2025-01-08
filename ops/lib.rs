// Copyright 2018-2025 the Deno authors. MIT license.

#![doc = include_str!("README.md")]
#![deny(clippy::unnecessary_wraps)]

use proc_macro::TokenStream;
use std::error::Error;

mod op2;
mod webidl;

/// A macro designed to provide an extremely fast V8->Rust interface layer.
#[doc = include_str!("op2/README.md")]
#[proc_macro_attribute]
pub fn op2(attr: TokenStream, item: TokenStream) -> TokenStream {
  op2_macro(attr, item)
}

fn op2_macro(attr: TokenStream, item: TokenStream) -> TokenStream {
  match crate::op2::op2(attr.into(), item.into()) {
    Ok(output) => output.into(),
    Err(err) => {
      let mut err: &dyn Error = &err;
      let mut output = "Failed to parse #[op2]:\n".to_owned();
      loop {
        output += &format!(" - {err}\n");
        if let Some(source) = err.source() {
          err = source;
        } else {
          break;
        }
      }
      panic!("{output}");
    }
  }
}

#[proc_macro_derive(WebIDL, attributes(webidl, options))]
pub fn webidl(item: TokenStream) -> TokenStream {
  match webidl::webidl(item.into()) {
    Ok(output) => output.into(),
    Err(err) => err.into_compile_error().into(),
  }
}
