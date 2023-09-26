// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use super::signature::Arg;
use super::signature::Buffer;
use super::signature::BufferMode;
use super::signature::NumericArg;
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

/// Given a V8Slice in `v8slice`, turns it into the appropriate slice type in `arg_ident`.
pub fn v8slice_to_buffer(
  deno_core: &TokenStream,
  arg_ident: &Ident,
  v8slice: &Ident,
  buffer: Buffer,
) -> Result<TokenStream, V8MappingError> {
  let make_arg = match buffer {
    Buffer::V8Slice(_, _) => {
      quote!(let #arg_ident = #arg_ident;)
    }
    Buffer::Slice(RefType::Ref, NumericArg::u8 | NumericArg::u32) => {
      quote!(let #arg_ident = #v8slice.as_ref();)
    }
    Buffer::Slice(RefType::Mut, NumericArg::u8 | NumericArg::u32) => {
      quote!(let #arg_ident = #v8slice.as_mut();)
    }
    Buffer::Ptr(RefType::Ref, NumericArg::u8 | NumericArg::u32) => {
      quote!(let #arg_ident = if #v8slice.len() == 0 { std::ptr::null() } else { #v8slice.as_ref().as_ptr() };)
    }
    Buffer::Ptr(RefType::Mut, NumericArg::u8 | NumericArg::u32) => {
      quote!(let #arg_ident = if #v8slice.len() == 0 { std::ptr::null_mut() } else { #v8slice.as_mut().as_mut_ptr() };)
    }
    Buffer::Vec(NumericArg::u8 | NumericArg::u32) => {
      quote!(let #arg_ident = #v8slice.to_vec();)
    }
    Buffer::BoxSlice(NumericArg::u8 | NumericArg::u32) => {
      quote!(let #arg_ident = #v8slice.to_boxed_slice();)
    }
    Buffer::Bytes(BufferMode::Copy) => {
      quote!(let #arg_ident = #v8slice.to_vec().into();)
    }
    Buffer::JsBuffer(BufferMode::Default | BufferMode::Detach) => {
      quote!(let #arg_ident = #deno_core::serde_v8::JsBuffer::from_parts(#v8slice);)
    }
    _ => {
      return Err(V8MappingError::NoMapping(
        "a v8slice argument",
        Arg::Buffer(buffer),
      ))
    }
  };
  Ok(make_arg)
}

pub fn fast_api_typed_array_to_buffer(
  deno_core: &TokenStream,
  arg_ident: &Ident,
  input: &Ident,
  buffer: Buffer,
) -> Result<TokenStream, V8MappingError> {
  let buf = quote!((
    // SAFETY: we are certain the implied lifetime is valid here as the slices never escape the
    // fastcall
    unsafe { #deno_core::v8::fast_api::FastApiTypedArray::get_storage_from_pointer_if_aligned(#input) }.expect("Invalid buffer"))
  );

  let res = match buffer {
    Buffer::Slice(_, NumericArg::u8 | NumericArg::u32) => {
      quote!(let #arg_ident = #buf;)
    }
    Buffer::Ptr(_, NumericArg::u8 | NumericArg::u32) => {
      quote!(let #arg_ident = if #buf.len() == 0 { ::std::ptr::null_mut() } else { #buf.as_mut_ptr() as _ };)
    }
    Buffer::Vec(NumericArg::u8 | NumericArg::u32) => {
      quote!(let #arg_ident = #buf.to_vec();)
    }
    Buffer::BoxSlice(NumericArg::u8 | NumericArg::u32) => {
      quote!(let #arg_ident = #buf.to_vec().into_boxed_slice();)
    }
    Buffer::Bytes(BufferMode::Copy) => {
      quote!(let #arg_ident = #buf.to_vec().into();)
    }
    _ => {
      return Err(V8MappingError::NoMapping(
        "a fast typed array buffer argument",
        Arg::Buffer(buffer),
      ))
    }
  };

  Ok(res)
}
