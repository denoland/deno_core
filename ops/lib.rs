// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#![doc = include_str!("README.md")]

use proc_macro::TokenStream;
use std::error::Error;

mod op;

/// A macro designed to provide an extremely fast V8->Rust interface layer.
#[doc = include_str!("op/README.md")]
#[proc_macro_attribute]
pub fn op(attr: TokenStream, item: TokenStream) -> TokenStream {
  op_macro(attr, item)
}

fn op_macro(attr: TokenStream, item: TokenStream) -> TokenStream {
  match crate::op::op(attr.into(), item.into()) {
    Ok(output) => output.into(),
    Err(err) => {
      let mut err: &dyn Error = &err;
      let mut output = "Failed to parse #[op]:\n".to_owned();
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
