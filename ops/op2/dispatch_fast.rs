// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use super::dispatch_shared::byte_slice_to_buffer;
use super::dispatch_shared::fast_api_typed_array_to_buffer;
use super::dispatch_shared::v8_intermediate_to_arg;
use super::dispatch_shared::v8_to_arg;
use super::dispatch_shared::v8slice_to_buffer;
use super::generator_state::gs_quote;
use super::generator_state::GeneratorState;
use super::signature::Arg;
use super::signature::BufferMode;
use super::signature::BufferSource;
use super::signature::BufferType;
use super::signature::NumericArg;
use super::signature::NumericFlag;
use super::signature::ParsedSignature;
use super::signature::RefType;
use super::signature::RetVal;
use super::signature::Special;
use super::signature::Strings;
use super::V8MappingError;
use super::V8SignatureMappingError;
use proc_macro2::Ident;
use proc_macro2::TokenStream;
use quote::format_ident;
use quote::quote;
use std::iter::zip;
use syn::Type;

#[allow(unused)]
#[derive(Debug, Default, PartialEq, Clone)]
pub(crate) enum V8FastCallType {
  #[default]
  Void,
  Bool,
  U32,
  I32,
  U64,
  I64,
  F32,
  F64,
  Pointer,
  V8Value,
  /// Any typed array.
  AnyArray,
  Uint8Array,
  Uint32Array,
  Float64Array,
  SeqOneByteString,
  CallbackOptions,
  /// ArrayBuffers are currently supported in fastcalls by passing a V8Value and manually unwrapping
  /// the buffer. In the future, V8 may be able to support ArrayBuffer fastcalls in the same way that
  /// a TypedArray overload works and we may be able to adjust the support here.
  ArrayBuffer,
  /// Used for virtual arguments that do not contribute a raw argument
  Virtual,
}

impl V8FastCallType {
  /// Quote fast value type.
  fn quote_rust_type(&self, deno_core: &TokenStream) -> TokenStream {
    match self {
      V8FastCallType::Void => quote!(()),
      V8FastCallType::Bool => quote!(bool),
      V8FastCallType::U32 => quote!(u32),
      V8FastCallType::I32 => quote!(i32),
      V8FastCallType::U64 => quote!(u64),
      V8FastCallType::I64 => quote!(i64),
      V8FastCallType::F32 => quote!(f32),
      V8FastCallType::F64 => quote!(f64),
      V8FastCallType::Pointer => quote!(*mut ::std::ffi::c_void),
      V8FastCallType::V8Value => {
        quote!(#deno_core::v8::Local<#deno_core::v8::Value>)
      }
      V8FastCallType::CallbackOptions => {
        quote!(*mut #deno_core::v8::fast_api::FastApiCallbackOptions)
      }
      V8FastCallType::SeqOneByteString => {
        quote!(*mut #deno_core::v8::fast_api::FastApiOneByteString)
      }
      V8FastCallType::Uint8Array => {
        quote!(*mut #deno_core::v8::fast_api::FastApiTypedArray<u8>)
      }
      V8FastCallType::Uint32Array => {
        quote!(*mut #deno_core::v8::fast_api::FastApiTypedArray<u32>)
      }
      V8FastCallType::Float64Array => {
        quote!(*mut #deno_core::v8::fast_api::FastApiTypedArray<f64>)
      }
      V8FastCallType::AnyArray | V8FastCallType::ArrayBuffer => {
        quote!(#deno_core::v8::Local<#deno_core::v8::Value>)
      }
      V8FastCallType::Virtual => unreachable!("invalid virtual argument"),
    }
  }

  /// Quote fast value type's variant.
  fn quote_ctype(&self) -> TokenStream {
    match &self {
      V8FastCallType::Void => quote!(CType::Void),
      V8FastCallType::Bool => quote!(CType::Bool),
      V8FastCallType::U32 => quote!(CType::Uint32),
      V8FastCallType::I32 => quote!(CType::Int32),
      V8FastCallType::U64 => quote!(CType::Uint64),
      V8FastCallType::I64 => quote!(CType::Int64),
      V8FastCallType::F32 => quote!(CType::Float32),
      V8FastCallType::F64 => quote!(CType::Float64),
      V8FastCallType::Pointer => quote!(CType::Pointer),
      V8FastCallType::V8Value => quote!(CType::V8Value),
      V8FastCallType::CallbackOptions => quote!(CType::CallbackOptions),
      V8FastCallType::AnyArray => unreachable!(),
      V8FastCallType::Uint8Array => unreachable!(),
      V8FastCallType::Uint32Array => unreachable!(),
      V8FastCallType::Float64Array => unreachable!(),
      V8FastCallType::SeqOneByteString => quote!(CType::SeqOneByteString),
      V8FastCallType::ArrayBuffer => unreachable!(),
      V8FastCallType::Virtual => unreachable!("invalid virtual argument"),
    }
  }

  /// Quote fast value type's variant.
  fn quote_type(&self) -> TokenStream {
    match &self {
      V8FastCallType::Void => quote!(Type::Void),
      V8FastCallType::Bool => quote!(Type::Bool),
      V8FastCallType::U32 => quote!(Type::Uint32),
      V8FastCallType::I32 => quote!(Type::Int32),
      V8FastCallType::U64 => quote!(Type::Uint64),
      V8FastCallType::I64 => quote!(Type::Int64),
      V8FastCallType::F32 => quote!(Type::Float32),
      V8FastCallType::F64 => quote!(Type::Float64),
      V8FastCallType::Pointer => quote!(Type::Pointer),
      V8FastCallType::V8Value => quote!(Type::V8Value),
      V8FastCallType::CallbackOptions => quote!(Type::CallbackOptions),
      V8FastCallType::AnyArray => quote!(Type::V8Value),
      V8FastCallType::Uint8Array => quote!(Type::TypedArray(CType::Uint8)),
      V8FastCallType::Uint32Array => quote!(Type::TypedArray(CType::Uint32)),
      V8FastCallType::Float64Array => quote!(Type::TypedArray(CType::Float64)),
      V8FastCallType::SeqOneByteString => quote!(Type::SeqOneByteString),
      V8FastCallType::ArrayBuffer => quote!(Type::V8Value),
      V8FastCallType::Virtual => unreachable!("invalid virtual argument"),
    }
  }
}

pub fn generate_dispatch_fast(
  generator_state: &mut GeneratorState,
  signature: &ParsedSignature,
) -> Result<
  Option<(TokenStream, TokenStream, TokenStream)>,
  V8SignatureMappingError,
> {
  enum Input<'a> {
    Concrete(V8FastCallType),
    Virtual(&'a Arg),
  }

  // Async not supported for fastcalls yet
  if signature.ret_val.is_async() {
    return Ok(None);
  }

  let mut inputs = vec![];
  for arg in &signature.args {
    let Some(fv) = map_arg_to_v8_fastcall_type(arg).map_err(|s| V8SignatureMappingError::NoArgMapping(s, arg.clone()))? else {
      return Ok(None);
    };
    if fv == V8FastCallType::Virtual {
      inputs.push(Input::Virtual(arg));
    } else {
      inputs.push(Input::Concrete(fv));
    }
  }

  let ret_val = match &signature.ret_val {
    RetVal::Infallible(arg) => arg,
    RetVal::Result(arg) => arg,
    _ => todo!(),
  };

  let output = match map_retval_to_v8_fastcall_type(ret_val).map_err(|s| {
    V8SignatureMappingError::NoRetValMapping(s, signature.ret_val.clone())
  })? {
    None => return Ok(None),
    Some(rv) => rv,
  };

  // Collect the names and types for the fastcall and the underlying op call
  let mut fastcall_names = vec![];
  let mut fastcall_types = vec![];
  let mut input_types = vec![];
  let mut call_names = vec![];
  let mut call_args = vec![];
  for (i, (input, arg)) in zip(inputs, &signature.args).enumerate() {
    let name = format_ident!("arg{i}");
    if let Input::Concrete(fv) = input {
      fastcall_names.push(name.clone());
      fastcall_types.push(fv.quote_rust_type(&generator_state.deno_core));
      input_types.push(fv.quote_type());
    }

    call_names.push(name.clone());
    call_args.push(
      map_v8_fastcall_arg_to_arg(generator_state, &name, arg)
        .map_err(|s| V8SignatureMappingError::NoArgMapping(s, arg.clone()))?,
    )
  }

  let GeneratorState {
    result,
    fast_function,
    fast_function_metrics,
    needs_fast_opctx,
    needs_fast_api_callback_options,
    needs_fast_js_runtime_state,
    ..
  } = generator_state;

  let handle_error = match signature.ret_val {
    RetVal::Infallible(_) => quote!(),
    RetVal::Result(_) => {
      *needs_fast_api_callback_options = true;
      *needs_fast_opctx = true;
      gs_quote!(generator_state(fast_api_callback_options, opctx) => {
        let #result = match #result {
          Ok(#result) => #result,
          Err(err) => {
            // FASTCALL FALLBACK: This is where we set the errors for the slow-call error pickup path. There
            // is no code running between this and the other FASTCALL FALLBACK comment, except some V8 code
            // required to perform the fallback process. This is why the below call is safe.

            // The reason we need to do this is because V8 does not allow exceptions to be thrown from the
            // fast call. Instead, you are required to set the fallback flag, which indicates to V8 that it
            // should re-call the slow version of the function. Technically the slow call should perform the
            // same operation and then throw the same error (because it should be idempotent), but in our
            // case we stash the error and pick it up on the slow path before doing any work.

            // TODO(mmastrac): We should allow an #[op] flag to re-perform slow calls without the error path when
            // the method is performance sensitive.

            // SAFETY: We guarantee that OpCtx has no mutable references once ops are live and being called,
            // allowing us to perform this one little bit of mutable magic.
            unsafe { #opctx.unsafely_set_last_error_for_ops_only(err); }
            #fast_api_callback_options.fallback = true;

            // SAFETY: All fast return types have zero as a valid value
            return unsafe { std::mem::zeroed() };
          }
        };
      })
    }
    _ => todo!(),
  };

  let with_js_runtime_state = if *needs_fast_js_runtime_state {
    *needs_fast_opctx = true;
    gs_quote!(generator_state(js_runtime_state, opctx) => {
      let #js_runtime_state = std::rc::Weak::upgrade(&#opctx.runtime_state).unwrap();
    })
  } else {
    quote!()
  };

  let with_opctx = if *needs_fast_opctx {
    *needs_fast_api_callback_options = true;
    gs_quote!(generator_state(deno_core, opctx, fast_api_callback_options) => {
      let #opctx = unsafe {
        &*(#deno_core::v8::Local::<#deno_core::v8::External>::cast(unsafe { #fast_api_callback_options.data.data }).value()
            as *const #deno_core::_ops::OpCtx)
      };
    })
  } else {
    quote!()
  };

  let mut fastcall_metrics_names = fastcall_names.clone();
  let mut fastcall_metrics_types = fastcall_types.clone();
  let mut input_types_metrics = input_types.clone();

  let with_fast_api_callback_options = if *needs_fast_api_callback_options {
    input_types.push(V8FastCallType::CallbackOptions.quote_type());
    fastcall_types.push(
      V8FastCallType::CallbackOptions
        .quote_rust_type(&generator_state.deno_core),
    );
    fastcall_names.push(generator_state.fast_api_callback_options.clone());
    gs_quote!(generator_state(fast_api_callback_options) => {
      let #fast_api_callback_options = unsafe { &mut *#fast_api_callback_options };
    })
  } else {
    quote!()
  };

  input_types_metrics.push(V8FastCallType::CallbackOptions.quote_type());
  fastcall_metrics_types.push(
    V8FastCallType::CallbackOptions.quote_rust_type(&generator_state.deno_core),
  );
  fastcall_metrics_names
    .push(generator_state.fast_api_callback_options.clone());

  let output_type = output.quote_ctype();

  let fast_definition = gs_quote!(generator_state(deno_core) => {
    use #deno_core::v8::fast_api::Type;
    use #deno_core::v8::fast_api::CType;
    #deno_core::v8::fast_api::FastFunction::new_with_bigint(
      &[ Type::V8Value, #( #input_types ),* ],
      #output_type,
      Self::#fast_function as *const ::std::ffi::c_void
    )
  });

  let fast_definition_metrics = gs_quote!(generator_state(deno_core) => {
    use #deno_core::v8::fast_api::Type;
    use #deno_core::v8::fast_api::CType;
    #deno_core::v8::fast_api::FastFunction::new_with_bigint(
      &[ Type::V8Value, #( #input_types_metrics ),* ],
      #output_type,
      Self::#fast_function_metrics as *const ::std::ffi::c_void
    )
  });

  // Ensure that we have the same types in the fast function definition as we do in the signature
  debug_assert!(fastcall_types.len() == input_types.len());

  let output_type = output.quote_rust_type(&generator_state.deno_core);
  // We don't want clippy to trigger warnings on number of arguments of the fastcall
  // function -- these will still trigger on our normal call function, however.
  let fast_fn = gs_quote!(generator_state(deno_core, fast_api_callback_options) => {
    #[allow(clippy::too_many_arguments)]
    fn #fast_function_metrics(
      this: #deno_core::v8::Local<#deno_core::v8::Object>,
      #( #fastcall_metrics_names: #fastcall_metrics_types, )*
    ) -> #output_type {
      let #fast_api_callback_options = unsafe { &mut *#fast_api_callback_options };
      let opctx = unsafe {
          &*(#deno_core::v8::Local::<#deno_core::v8::External>::cast(
            unsafe { #fast_api_callback_options.data.data }
          ).value() as *const #deno_core::_ops::OpCtx)
      };
      #deno_core::_ops::dispatch_metrics_fast(&opctx, #deno_core::_ops::OpMetricsEvent::Dispatched);
      let res = Self::#fast_function( this, #( #fastcall_names, )* );
      #deno_core::_ops::dispatch_metrics_fast(&opctx, #deno_core::_ops::OpMetricsEvent::Completed);
      res
    }

    #[allow(clippy::too_many_arguments)]
    fn #fast_function(
      _: #deno_core::v8::Local<#deno_core::v8::Object>,
      #( #fastcall_names: #fastcall_types, )*
    ) -> #output_type {
      #with_fast_api_callback_options
      #with_opctx
      #with_js_runtime_state
      let #result = {
        #(#call_args)*
        Self::call(#(#call_names),*)
      };
      #handle_error
      // Result may need a simple cast (eg: SMI u32->i32)
      #result as _
    }
  });

  Ok(Some((fast_definition, fast_definition_metrics, fast_fn)))
}

#[allow(clippy::too_many_arguments)]
fn map_v8_fastcall_arg_to_arg(
  generator_state: &mut GeneratorState,
  arg_ident: &Ident,
  arg: &Arg,
) -> Result<TokenStream, V8MappingError> {
  let GeneratorState {
    deno_core,
    opctx,
    js_runtime_state,
    fast_api_callback_options,
    needs_fast_opctx: needs_opctx,
    needs_fast_api_callback_options,
    needs_fast_js_runtime_state: needs_js_runtime_state,
    ..
  } = generator_state;

  let arg_temp = format_ident!("{}_temp", arg_ident);

  let res = match arg {
    Arg::Buffer(
      buffer @ (BufferType::V8Slice(..) | BufferType::JsBuffer),
      _,
      BufferSource::ArrayBuffer,
    ) => {
      *needs_fast_api_callback_options = true;
      let buf = v8slice_to_buffer(deno_core, arg_ident, &arg_temp, *buffer)?;
      quote!(
        let Ok(mut #arg_temp) = #deno_core::_ops::to_v8_slice_buffer(#arg_ident.into()) else {
          #fast_api_callback_options.fallback = true;
          // SAFETY: All fast return types have zero as a valid value
          return unsafe { std::mem::zeroed() };
        };
        #buf
      )
    }
    Arg::Buffer(
      buffer @ (BufferType::V8Slice(..) | BufferType::JsBuffer),
      _,
      BufferSource::TypedArray,
    ) => {
      *needs_fast_api_callback_options = true;
      let buf = v8slice_to_buffer(deno_core, arg_ident, &arg_temp, *buffer)?;
      quote!(
        let Ok(mut #arg_temp) = #deno_core::_ops::to_v8_slice(#arg_ident.into()) else {
          #fast_api_callback_options.fallback = true;
          // SAFETY: All fast return types have zero as a valid value
          return unsafe { std::mem::zeroed() };
        };
        #buf
      )
    }
    Arg::Buffer(buffer, _, BufferSource::ArrayBuffer) => {
      *needs_fast_api_callback_options = true;
      let buf = byte_slice_to_buffer(arg_ident, &arg_temp, *buffer)?;
      quote!(
        // SAFETY: This slice doesn't outlive the function
        let Ok(mut #arg_temp) = (unsafe { #deno_core::_ops::to_slice_buffer(#arg_ident.into()) }) else {
          #fast_api_callback_options.fallback = true;
          // SAFETY: All fast return types have zero as a valid value
          return unsafe { std::mem::zeroed() };
        };
        #buf
      )
    }
    Arg::Buffer(buffer, _, BufferSource::Any) => {
      *needs_fast_api_callback_options = true;
      let buf = byte_slice_to_buffer(arg_ident, &arg_temp, *buffer)?;
      quote!(
        // SAFETY: This slice doesn't outlive the function
        let Ok(mut #arg_temp) = (unsafe { #deno_core::_ops::to_slice_buffer_any(#arg_ident.into()) }) else {
          #fast_api_callback_options.fallback = true;
          // SAFETY: All fast return types have zero as a valid value
          return unsafe { std::mem::zeroed() };
        };
        #buf
      )
    }
    Arg::Buffer(buffer, _, BufferSource::TypedArray) => {
      fast_api_typed_array_to_buffer(deno_core, arg_ident, arg_ident, *buffer)?
    }
    Arg::Special(Special::Isolate) => {
      *needs_opctx = true;
      quote!(let #arg_ident = #opctx.isolate;)
    }
    Arg::Ref(RefType::Ref, Special::OpState) => {
      *needs_opctx = true;
      quote!(let #arg_ident = &::std::cell::RefCell::borrow(&#opctx.state);)
    }
    Arg::Ref(RefType::Mut, Special::OpState) => {
      *needs_opctx = true;
      quote!(let #arg_ident = &mut ::std::cell::RefCell::borrow_mut(&#opctx.state);)
    }
    Arg::RcRefCell(Special::OpState) => {
      *needs_opctx = true;
      quote!(let #arg_ident = #opctx.state.clone();)
    }
    Arg::Ref(RefType::Ref, Special::JsRuntimeState) => {
      *needs_js_runtime_state = true;
      quote!(let #arg_ident = &::std::cell::RefCell::borrow(&#js_runtime_state);)
    }
    Arg::Ref(RefType::Mut, Special::JsRuntimeState) => {
      *needs_js_runtime_state = true;
      quote!(let #arg_ident = &mut ::std::cell::RefCell::borrow_mut(&#js_runtime_state);)
    }
    Arg::RcRefCell(Special::JsRuntimeState) => {
      *needs_js_runtime_state = true;
      quote!(let #arg_ident = #js_runtime_state.clone();)
    }
    Arg::State(RefType::Ref, state) => {
      *needs_opctx = true;
      let state =
        syn::parse_str::<Type>(state).expect("Failed to reparse state type");
      quote! {
        let #arg_ident = ::std::cell::RefCell::borrow(&#opctx.state);
        let #arg_ident = #deno_core::_ops::opstate_borrow::<#state>(&#arg_ident);
      }
    }
    Arg::State(RefType::Mut, state) => {
      *needs_opctx = true;
      let state =
        syn::parse_str::<Type>(state).expect("Failed to reparse state type");
      quote! {
        let mut #arg_ident = ::std::cell::RefCell::borrow_mut(&#opctx.state);
        let #arg_ident = #deno_core::_ops::opstate_borrow_mut::<#state>(&mut #arg_ident);
      }
    }
    Arg::OptionState(RefType::Ref, state) => {
      *needs_opctx = true;
      let state =
        syn::parse_str::<Type>(state).expect("Failed to reparse state type");
      quote! {
        let #arg_ident = &::std::cell::RefCell::borrow(&#opctx.state);
        let #arg_ident = #arg_ident.try_borrow::<#state>();
      }
    }
    Arg::OptionState(RefType::Mut, state) => {
      *needs_opctx = true;
      let state =
        syn::parse_str::<Type>(state).expect("Failed to reparse state type");
      quote! {
        let mut #arg_ident = &mut ::std::cell::RefCell::borrow_mut(&#opctx.state);
        let #arg_ident = #arg_ident.try_borrow_mut::<#state>();
      }
    }
    Arg::String(Strings::RefStr) => {
      quote! {
        let mut #arg_temp: [::std::mem::MaybeUninit<u8>; #deno_core::_ops::STRING_STACK_BUFFER_SIZE] = [::std::mem::MaybeUninit::uninit(); #deno_core::_ops::STRING_STACK_BUFFER_SIZE];
        let #arg_ident = &#deno_core::_ops::to_str_ptr(unsafe { &mut *#arg_ident }, &mut #arg_temp);
      }
    }
    Arg::String(Strings::String) => {
      quote!(let #arg_ident = #deno_core::_ops::to_string_ptr(unsafe { &mut *#arg_ident });)
    }
    Arg::String(Strings::CowStr) => {
      quote! {
        let mut #arg_temp: [::std::mem::MaybeUninit<u8>; #deno_core::_ops::STRING_STACK_BUFFER_SIZE] = [::std::mem::MaybeUninit::uninit(); #deno_core::_ops::STRING_STACK_BUFFER_SIZE];
        let #arg_ident = #deno_core::_ops::to_str_ptr(unsafe { &mut *#arg_ident }, &mut #arg_temp);
      }
    }
    Arg::String(Strings::CowByte) => {
      quote!(let #arg_ident = #deno_core::_ops::to_cow_byte_ptr(unsafe { &mut *#arg_ident });)
    }
    Arg::V8Local(v8)
    | Arg::OptionV8Local(v8)
    | Arg::V8Ref(_, v8)
    | Arg::OptionV8Ref(_, v8) => {
      let arg_ident = arg_ident.clone();
      let deno_core = deno_core.clone();
      // Note that we only request callback options if we were required to provide a type error
      let throw_type_error = || {
        *needs_fast_api_callback_options = true;
        Ok(quote! {
          #fast_api_callback_options.fallback = true;
          // SAFETY: All fast return types have zero as a valid value
          return unsafe { std::mem::zeroed() };
        })
      };
      let extract_intermediate = v8_intermediate_to_arg(&arg_ident, arg);
      v8_to_arg(
        v8,
        &arg_ident,
        arg,
        &deno_core,
        throw_type_error,
        extract_intermediate,
      )?
    }
    _ => quote!(let #arg_ident = #arg_ident as _;),
  };
  Ok(res)
}

fn map_arg_to_v8_fastcall_type(
  arg: &Arg,
) -> Result<Option<V8FastCallType>, V8MappingError> {
  let rv = match arg {
    // We don't allow detaching buffers in fast mode
    Arg::Buffer(_, BufferMode::Detach, _) => return Ok(None),
    // We don't allow JsBuffer or V8Slice fastcalls for TypedArray
    // TODO(mmastrac): we can enable these soon
    Arg::Buffer(
      BufferType::JsBuffer | BufferType::V8Slice(..),
      _,
      BufferSource::TypedArray,
    ) => V8FastCallType::V8Value,
    Arg::Buffer(
      BufferType::Slice(.., NumericArg::u8)
      | BufferType::Ptr(.., NumericArg::u8),
      _,
      BufferSource::Any,
    ) => V8FastCallType::AnyArray,
    // TODO(mmastrac): implement fast for any Any-typed buffer
    Arg::Buffer(_, _, BufferSource::Any) => return Ok(None),
    Arg::Buffer(_, _, BufferSource::ArrayBuffer) => V8FastCallType::ArrayBuffer,
    Arg::Buffer(
      BufferType::Slice(.., NumericArg::u32)
      | BufferType::Ptr(.., NumericArg::u32)
      | BufferType::Vec(.., NumericArg::u32)
      | BufferType::BoxSlice(.., NumericArg::u32),
      _,
      BufferSource::TypedArray,
    ) => V8FastCallType::Uint32Array,
    Arg::Buffer(
      BufferType::Slice(.., NumericArg::f64)
      | BufferType::Ptr(.., NumericArg::f64)
      | BufferType::Vec(.., NumericArg::f64)
      | BufferType::BoxSlice(.., NumericArg::f64),
      _,
      BufferSource::TypedArray,
    ) => V8FastCallType::Float64Array,
    Arg::Buffer(_, _, BufferSource::TypedArray) => V8FastCallType::Uint8Array,
    // Virtual OpState arguments
    Arg::RcRefCell(Special::OpState)
    | Arg::Ref(_, Special::OpState)
    | Arg::RcRefCell(Special::JsRuntimeState)
    | Arg::Ref(_, Special::JsRuntimeState)
    | Arg::State(..)
    | Arg::Special(Special::Isolate)
    | Arg::OptionState(..) => V8FastCallType::Virtual,
    // Other types + ref types are not handled
    Arg::OptionNumeric(..)
    | Arg::Option(_)
    | Arg::OptionString(_)
    | Arg::OptionBuffer(..)
    | Arg::SerdeV8(_)
    | Arg::Ref(..) => return Ok(None),
    // We don't support v8 global arguments
    Arg::V8Global(_) => return Ok(None),
    // We do support v8 type arguments (including Option<...>)
    Arg::V8Ref(RefType::Ref, _)
    | Arg::V8Local(_)
    | Arg::OptionV8Local(_)
    | Arg::OptionV8Ref(RefType::Ref, _) => V8FastCallType::V8Value,

    Arg::Numeric(NumericArg::bool, _) => V8FastCallType::Bool,
    Arg::Numeric(NumericArg::u32, _)
    | Arg::Numeric(NumericArg::u16, _)
    | Arg::Numeric(NumericArg::u8, _) => V8FastCallType::U32,
    Arg::Numeric(NumericArg::i32, _)
    | Arg::Numeric(NumericArg::i16, _)
    | Arg::Numeric(NumericArg::i8, _)
    | Arg::Numeric(NumericArg::__SMI__, _) => V8FastCallType::I32,
    Arg::Numeric(NumericArg::u64 | NumericArg::usize, NumericFlag::None) => {
      V8FastCallType::U64
    }
    Arg::Numeric(NumericArg::i64 | NumericArg::isize, NumericFlag::None) => {
      V8FastCallType::I64
    }
    Arg::Numeric(
      NumericArg::u64 | NumericArg::usize | NumericArg::i64 | NumericArg::isize,
      NumericFlag::Number,
    ) => V8FastCallType::F64,
    Arg::Numeric(NumericArg::f32, _) => V8FastCallType::F32,
    Arg::Numeric(NumericArg::f64, _) => V8FastCallType::F64,
    // Ref strings that are one byte internally may be passed as a SeqOneByteString,
    // which gives us a FastApiOneByteString.
    Arg::String(Strings::RefStr) => V8FastCallType::SeqOneByteString,
    // Owned strings can be fast, but we'll have to copy them.
    Arg::String(Strings::String) => V8FastCallType::SeqOneByteString,
    // Cow strings can be fast, but may require copying
    Arg::String(Strings::CowStr) => V8FastCallType::SeqOneByteString,
    // Cow byte strings can be fast and don't require copying
    Arg::String(Strings::CowByte) => V8FastCallType::SeqOneByteString,
    Arg::External(..) => V8FastCallType::Pointer,
    _ => return Err("a fast argument"),
  };
  Ok(Some(rv))
}

fn map_retval_to_v8_fastcall_type(
  arg: &Arg,
) -> Result<Option<V8FastCallType>, V8MappingError> {
  let rv = match arg {
    Arg::OptionNumeric(..) | Arg::SerdeV8(_) => return Ok(None),
    Arg::Void => V8FastCallType::Void,
    Arg::Numeric(NumericArg::bool, _) => V8FastCallType::Bool,
    Arg::Numeric(NumericArg::u32, _)
    | Arg::Numeric(NumericArg::u16, _)
    | Arg::Numeric(NumericArg::u8, _) => V8FastCallType::U32,
    Arg::Numeric(NumericArg::__SMI__, _)
    | Arg::Numeric(NumericArg::i32, _)
    | Arg::Numeric(NumericArg::i16, _)
    | Arg::Numeric(NumericArg::i8, _) => V8FastCallType::I32,
    Arg::Numeric(NumericArg::u64 | NumericArg::usize, NumericFlag::None) => {
      V8FastCallType::U64
    }
    Arg::Numeric(NumericArg::i64 | NumericArg::isize, NumericFlag::None) => {
      V8FastCallType::I64
    }
    Arg::Numeric(
      NumericArg::u64 | NumericArg::usize | NumericArg::i64 | NumericArg::isize,
      NumericFlag::Number,
    ) => V8FastCallType::F64,
    Arg::Numeric(NumericArg::f32, _) => V8FastCallType::F32,
    Arg::Numeric(NumericArg::f64, _) => V8FastCallType::F64,
    // We don't return special return types
    Arg::Option(_) => return Ok(None),
    Arg::OptionString(_) => return Ok(None),
    Arg::Special(_) => return Ok(None),
    Arg::String(_) => return Ok(None),
    // We don't support returning v8 types
    Arg::V8Ref(..)
    | Arg::V8Global(_)
    | Arg::V8Local(_)
    | Arg::OptionV8Local(_)
    | Arg::OptionV8Ref(..) => return Ok(None),
    Arg::Buffer(..) | Arg::OptionBuffer(..) => return Ok(None),
    Arg::External(..) => V8FastCallType::Pointer,
    _ => return Err("a fast return value"),
  };
  Ok(Some(rv))
}
