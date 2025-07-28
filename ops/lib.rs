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

#[cfg(test)]
mod infra {
  use std::path::PathBuf;
  use syn::File;

  pub fn run_macro_expansion_test<F, I>(input: PathBuf, expander: F)
  where
    F: FnOnce(File) -> I,
    I: Iterator<Item = proc_macro2::TokenStream>,
  {
    let update_expected = std::env::var("UPDATE_EXPECTED").is_ok();

    let source =
      std::fs::read_to_string(&input).expect("Failed to read test file");

    const PRELUDE: &str = r"// Copyright 2018-2025 the Deno authors. MIT license.

#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();";

    if !source.starts_with(PRELUDE) {
      panic!("Source does not start with expected prelude:]n{PRELUDE}");
    }

    let file =
      syn::parse_str::<File>(&source).expect("Failed to parse Rust file");

    let expected_out = expander(file)
      .map(|tokens| {
        println!("======== Raw tokens ========:\n{}", tokens.clone());
        let tree = syn::parse2(tokens).unwrap();
        let actual = prettyplease::unparse(&tree);
        println!("======== Generated ========:\n{}", actual);
        actual
      })
      .collect::<Vec<String>>()
      .join("\n");

    if update_expected {
      std::fs::write(input.with_extension("out"), expected_out)
        .expect("Failed to write expectation file");
    } else {
      let expected = std::fs::read_to_string(input.with_extension("out"))
        .expect("Failed to read expectation file");

      pretty_assertions::assert_eq!(
        expected,
        expected_out,
        "Failed to match expectation. Use UPDATE_EXPECTED=1."
      );
    }
  }
}
