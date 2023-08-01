// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use super::signature::Arg;
use super::signature::RefType;
use super::signature::V8Arg;
use super::V8MappingError;
use proc_macro2::Ident;
use proc_macro2::TokenStream;
use quote::format_ident;
use quote::quote;

/// Given an [`Arg`] containing a V8 value, converts this value to its final argument form.
pub fn v8_intermediate_to_arg(i: &Ident, arg: &Arg) -> TokenStream {
  let arg = match arg {
    Arg::V8Ref(RefType::Ref, _) => quote!(&#i),
    Arg::V8Ref(RefType::Mut, _) => quote!(::std::ops::DerefMut::deref_mut(#i)),
    Arg::V8Local(_) => quote!(#i),
    Arg::OptionV8Ref(RefType::Ref, _) => {
      quote!(match &#i { None => None, Some(v) => Some(::std::ops::Deref::deref(v)) })
    }
    Arg::OptionV8Ref(RefType::Mut, _) => {
      quote!(match &#i { None => None, Some(v) => Some(::std::ops::DerefMut::deref_mut(v)) })
    }
    Arg::OptionV8Local(_) => quote!(#i),
    _ => unreachable!("Not a v8 local/ref arg: {arg:?}"),
  };
  quote!(let #i = #arg;)
}

/// Given an [`Arg`] containing a V8 value, converts this value to its final argument form.
pub fn v8_intermediate_to_global_arg(
  deno_core: &TokenStream,
  isolate: &Ident,
  i: &Ident,
  arg: &Arg,
) -> TokenStream {
  let arg = match arg {
    Arg::V8Global(_) => quote!(#deno_core::v8::Global::new(&mut #isolate, #i)),
    Arg::OptionV8Global(_) => {
      quote!(#deno_core::v8::Global::new(&mut #isolate, #i))
    }
    _ => unreachable!("Not a v8 global arg: {arg:?}"),
  };
  quote!(let #i = #arg;)
}

/// Generates a [`v8::Value`] of the correct type for the required V8Arg, throwing an exception if the
/// type cannot be cast.
pub fn v8_to_arg(
  v8: &V8Arg,
  arg_ident: &Ident,
  arg: &Arg,
  deno_core: &TokenStream,
  mut throw_type_error: impl FnMut() -> Result<TokenStream, V8MappingError>,
  extract_intermediate: TokenStream,
) -> Result<TokenStream, V8MappingError> {
  let try_convert = format_ident!(
    "{}",
    if arg.is_option() {
      "v8_try_convert_option"
    } else {
      "v8_try_convert"
    }
  );
  let throw_type_error_block = if *v8 == V8Arg::Value {
    quote!(unreachable!())
  } else {
    throw_type_error()?
  };
  Ok(quote! {
    let Ok(mut #arg_ident) = #deno_core::_ops::#try_convert::<#deno_core::v8::#v8>(#arg_ident) else {
      #throw_type_error_block
    };
    #extract_intermediate
  })
}
