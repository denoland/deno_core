// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use super::dispatch_shared::v8_intermediate_to_arg;
use super::dispatch_shared::v8_intermediate_to_global_arg;
use super::dispatch_shared::v8_to_arg;
use super::generator_state::GeneratorState;
use super::signature::Arg;
use super::signature::Buffer;
use super::signature::NumericArg;
use super::signature::ParsedSignature;
use super::signature::RefType;
use super::signature::RetVal;
use super::signature::Special;
use super::MacroConfig;
use super::V8MappingError;
use crate::op2::generator_state::gs_extract;
use crate::op2::generator_state::gs_quote;
use proc_macro2::Ident;
use proc_macro2::TokenStream;
use quote::format_ident;
use quote::quote;
use syn2::Type;

pub(crate) fn generate_dispatch_slow(
  config: &MacroConfig,
  generator_state: &mut GeneratorState,
  signature: &ParsedSignature,
) -> Result<TokenStream, V8MappingError> {
  let mut output = TokenStream::new();

  // Fast ops require the slow op to check op_ctx for the last error
  if config.fast && matches!(signature.ret_val, RetVal::Result(_)) {
    generator_state.needs_opctx = true;
    let throw_exception = throw_exception(generator_state)?;
    // If the fast op returned an error, we must throw it rather than doing work.
    output.extend(quote!{
      // FASTCALL FALLBACK: This is where we pick up the errors for the slow-call error pickup
      // path. There is no code running between this and the other FASTCALL FALLBACK comment,
      // except some V8 code required to perform the fallback process. This is why the below call is safe.

      // SAFETY: We guarantee that OpCtx has no mutable references once ops are live and being called,
      // allowing us to perform this one little bit of mutable magic.
      if let Some(err) = unsafe { opctx.unsafely_take_last_error_for_ops_only() } {
        #throw_exception
      }
    });
  }

  // Collect virtual arguments in a deferred list that we compute at the very end. This allows us to borrow
  // the scope/opstate in the intermediate stages.
  let mut deferred = TokenStream::new();
  let mut input_index = 0;

  for (index, arg) in signature.args.iter().enumerate() {
    if arg.is_virtual() {
      deferred.extend(from_arg(generator_state, index, arg)?);
    } else {
      output.extend(extract_arg(generator_state, index, input_index)?);
      output.extend(from_arg(generator_state, index, arg)?);
      input_index += 1;
    }
  }

  output.extend(deferred);
  output.extend(call(generator_state)?);
  output.extend(return_value(generator_state, &signature.ret_val)?);

  let with_scope = if generator_state.needs_scope {
    with_scope(generator_state)
  } else {
    quote!()
  };

  let with_opstate = if generator_state.needs_opstate {
    with_opstate(generator_state)
  } else {
    quote!()
  };

  let with_opctx = if generator_state.needs_opctx {
    with_opctx(generator_state)
  } else {
    quote!()
  };

  let with_retval = if generator_state.needs_retval {
    with_retval(generator_state)
  } else {
    quote!()
  };

  let with_args = if generator_state.needs_args {
    with_fn_args(generator_state)
  } else {
    quote!()
  };

  Ok(
    gs_quote!(generator_state(deno_core, info, slow_function) => {
      extern "C" fn #slow_function(#info: *const #deno_core::v8::FunctionCallbackInfo) {
        #with_scope
        #with_retval
        #with_args
        #with_opctx
        #with_opstate

        #output
      }
    }),
  )
}

pub(crate) fn with_scope(generator_state: &mut GeneratorState) -> TokenStream {
  gs_quote!(generator_state(deno_core, info, scope) =>
    (let mut #scope = unsafe { #deno_core::v8::CallbackScope::new(&*#info) };)
  )
}

pub(crate) fn with_retval(generator_state: &mut GeneratorState) -> TokenStream {
  gs_quote!(generator_state(deno_core, retval, info) =>
    (let mut #retval = #deno_core::v8::ReturnValue::from_function_callback_info(unsafe { &*#info });)
  )
}

pub(crate) fn with_fn_args(
  generator_state: &mut GeneratorState,
) -> TokenStream {
  gs_quote!(generator_state(deno_core, info, fn_args) =>
    (let #fn_args = #deno_core::v8::FunctionCallbackArguments::from_function_callback_info(unsafe { &*#info });)
  )
}

pub(crate) fn with_opctx(generator_state: &mut GeneratorState) -> TokenStream {
  generator_state.needs_args = true;
  gs_quote!(generator_state(deno_core, opctx, fn_args) =>
    (let #opctx = unsafe {
    &*(#deno_core::v8::Local::<#deno_core::v8::External>::cast(#fn_args.data()).value()
        as *const #deno_core::_ops::OpCtx)
    };)
  )
}

pub(crate) fn with_opstate(
  generator_state: &mut GeneratorState,
) -> TokenStream {
  generator_state.needs_opctx = true;
  gs_quote!(generator_state(opctx, opstate) =>
    (let #opstate = &#opctx.state;)
  )
}

pub fn extract_arg(
  generator_state: &mut GeneratorState,
  index: usize,
  input_index: usize,
) -> Result<TokenStream, V8MappingError> {
  let GeneratorState { fn_args, .. } = &generator_state;
  let arg_ident = generator_state.args.get(index);

  Ok(quote!(
    let #arg_ident = #fn_args.get(#input_index as i32);
  ))
}

pub fn from_arg(
  mut generator_state: &mut GeneratorState,
  index: usize,
  arg: &Arg,
) -> Result<TokenStream, V8MappingError> {
  let GeneratorState {
    deno_core,
    args,
    scope,
    opstate,
    needs_scope,
    needs_opstate,
    ..
  } = &mut generator_state;
  let arg_ident = args
    .get(index)
    .expect("Argument at index was missing")
    .clone();
  let arg_temp = format_ident!("{}_temp", arg_ident);
  let res = match arg {
    Arg::Numeric(NumericArg::bool) => quote! {
      let #arg_ident = #arg_ident.is_true();
    },
    Arg::Numeric(NumericArg::u8)
    | Arg::Numeric(NumericArg::u16)
    | Arg::Numeric(NumericArg::u32) => {
      quote! {
        let #arg_ident = #deno_core::_ops::to_u32(&#arg_ident) as _;
      }
    }
    Arg::Numeric(NumericArg::i8)
    | Arg::Numeric(NumericArg::i16)
    | Arg::Numeric(NumericArg::i32)
    | Arg::Numeric(NumericArg::__SMI__) => {
      quote! {
        let #arg_ident = #deno_core::_ops::to_i32(&#arg_ident) as _;
      }
    }
    Arg::Numeric(NumericArg::u64) | Arg::Numeric(NumericArg::usize) => {
      quote! {
        let #arg_ident = #deno_core::_ops::to_u64(&#arg_ident) as _;
      }
    }
    Arg::Numeric(NumericArg::i64) | Arg::Numeric(NumericArg::isize) => {
      quote! {
        let #arg_ident = #deno_core::_ops::to_i64(&#arg_ident) as _;
      }
    }
    Arg::Numeric(NumericArg::f32) => {
      quote! {
        let #arg_ident = #deno_core::_ops::to_f32(&#arg_ident) as _;
      }
    }
    Arg::Numeric(NumericArg::f64) => {
      quote! {
        let #arg_ident = #deno_core::_ops::to_f64(&#arg_ident) as _;
      }
    }
    Arg::OptionNumeric(numeric) => {
      let some = from_arg(generator_state, index, &Arg::Numeric(*numeric))?;
      quote! {
        let #arg_ident = if #arg_ident.is_null_or_undefined() {
          None
        } else {
          #some
          Some(#arg_ident)
        };
      }
    }
    Arg::Option(Special::String) => {
      *needs_scope = true;
      quote! {
        let #arg_ident = #arg_ident.to_rust_string_lossy(&mut #scope);
      }
    }
    Arg::Special(Special::String) => {
      *needs_scope = true;
      quote! {
        let #arg_ident = #arg_ident.to_rust_string_lossy(&mut #scope);
      }
    }
    Arg::Special(Special::RefStr) => {
      *needs_scope = true;
      quote! {
        // Trade stack space for potentially non-allocating strings
        let mut #arg_temp: [::std::mem::MaybeUninit<u8>; #deno_core::_ops::STRING_STACK_BUFFER_SIZE] = [::std::mem::MaybeUninit::uninit(); #deno_core::_ops::STRING_STACK_BUFFER_SIZE];
        let #arg_ident = &#deno_core::_ops::to_str(&mut #scope, &#arg_ident, &mut #arg_temp);
      }
    }
    Arg::Special(Special::CowStr) => {
      *needs_scope = true;
      quote! {
        // Trade stack space for potentially non-allocating strings
        let mut #arg_temp: [::std::mem::MaybeUninit<u8>; #deno_core::_ops::STRING_STACK_BUFFER_SIZE] = [::std::mem::MaybeUninit::uninit(); #deno_core::_ops::STRING_STACK_BUFFER_SIZE];
        let #arg_ident = #deno_core::_ops::to_str(&mut #scope, &#arg_ident, &mut #arg_temp);
      }
    }
    Arg::Buffer(buffer) => {
      from_arg_buffer(generator_state, &arg_ident, buffer)?
    }
    Arg::Ref(_, Special::HandleScope) => {
      *needs_scope = true;
      quote!(let #arg_ident = &mut #scope;)
    }
    Arg::Ref(RefType::Ref, Special::OpState) => {
      *needs_opstate = true;
      quote!(let #arg_ident = &#opstate.borrow();)
    }
    Arg::Ref(RefType::Mut, Special::OpState) => {
      *needs_opstate = true;
      quote!(let #arg_ident = &mut #opstate.borrow_mut();)
    }
    Arg::RcRefCell(Special::OpState) => {
      *needs_opstate = true;
      quote!(let #arg_ident = #opstate.clone();)
    }
    Arg::State(RefType::Ref, state) => {
      *needs_opstate = true;
      let state =
        syn2::parse_str::<Type>(state).expect("Failed to reparse state type");
      quote! {
        let #arg_ident = #opstate.borrow();
        let #arg_ident = #arg_ident.borrow::<#state>();
      }
    }
    Arg::State(RefType::Mut, state) => {
      *needs_opstate = true;
      let state =
        syn2::parse_str::<Type>(state).expect("Failed to reparse state type");
      quote! {
        let mut #arg_ident = #opstate.borrow_mut();
        let #arg_ident = #arg_ident.borrow_mut::<#state>();
      }
    }
    Arg::OptionState(RefType::Ref, state) => {
      *needs_opstate = true;
      let state =
        syn2::parse_str::<Type>(state).expect("Failed to reparse state type");
      quote! {
        let #arg_ident = #opstate.borrow();
        let #arg_ident = #arg_ident.try_borrow::<#state>();
      }
    }
    Arg::OptionState(RefType::Mut, state) => {
      *needs_opstate = true;
      let state =
        syn2::parse_str::<Type>(state).expect("Failed to reparse state type");
      quote! {
        let mut #arg_ident = #opstate.borrow_mut();
        let #arg_ident = #arg_ident.try_borrow_mut::<#state>();
      }
    }
    Arg::V8Local(v8)
    | Arg::OptionV8Local(v8)
    | Arg::V8Ref(RefType::Ref, v8)
    | Arg::OptionV8Ref(RefType::Ref, v8) => {
      let deno_core = deno_core.clone();
      let throw_type_error =
        || throw_type_error(generator_state, format!("expected {v8:?}"));
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
    Arg::V8Global(v8) | Arg::OptionV8Global(v8) => {
      *needs_scope = true;
      let deno_core = deno_core.clone();
      let scope = scope.clone();
      let throw_type_error =
        || throw_type_error(generator_state, format!("expected {v8:?}"));
      let extract_intermediate =
        v8_intermediate_to_global_arg(&deno_core, &scope, &arg_ident, arg);
      v8_to_arg(
        v8,
        &arg_ident,
        arg,
        &deno_core,
        throw_type_error,
        extract_intermediate,
      )?
    }
    Arg::SerdeV8(_class) => {
      *needs_scope = true;
      let deno_core = deno_core.clone();
      let scope = scope.clone();
      let err = format_ident!("{}_err", arg_ident);
      let throw_exception = throw_type_error_string(generator_state, &err)?;
      quote! {
        let #arg_ident = match #deno_core::_ops::serde_v8_to_rust(&mut #scope, #arg_ident) {
          Ok(t) => t,
          Err(#err) => {
            #throw_exception;
          }
        };
      }
    }
    _ => return Err(V8MappingError::NoMapping("a slow argument", arg.clone())),
  };
  Ok(res)
}

pub fn from_arg_buffer(
  generator_state: &mut GeneratorState,
  arg_ident: &Ident,
  buffer: &Buffer,
) -> Result<TokenStream, V8MappingError> {
  let err = format_ident!("{}_err", arg_ident);
  let throw_exception = throw_type_error_static_string(generator_state, &err)?;

  let GeneratorState {
    deno_core,
    scope,
    needs_scope,
    ..
  } = generator_state;

  *needs_scope = true;

  // TODO(mmastrac): Other buffer types
  let array = NumericArg::u8
    .v8_array_type()
    .expect("Could not retrieve the v8 type");

  let make_v8slice = quote! {
    let mut #arg_ident = match unsafe { #deno_core::_ops::to_v8_slice::<#deno_core::v8::#array>(&mut #scope, #arg_ident) } {
      Ok(#arg_ident) => #arg_ident,
      Err(#err) => {
        #throw_exception
      }
    };
  };

  let make_arg = match buffer {
    Buffer::Slice(_, NumericArg::u8) => {
      quote!(let #arg_ident = &mut #arg_ident;)
    }
    Buffer::Vec(NumericArg::u8) => {
      quote!(let #arg_ident = #arg_ident.to_vec();)
    }
    Buffer::BoxSlice(NumericArg::u8) => {
      quote!(let #arg_ident = #arg_ident.to_boxed_slice();)
    }
    Buffer::Bytes => {
      // TODO(mmastrac): This assumes #[buffer(copy)]
      quote!(let #arg_ident = #arg_ident.to_vec().into();)
    }
    _ => {
      return Err(V8MappingError::NoMapping(
        "a buffer argument",
        Arg::Buffer(*buffer),
      ))
    }
  };

  Ok(quote! {
    #make_v8slice
    #make_arg
  })
}

pub fn call(
  generator_state: &mut GeneratorState,
) -> Result<TokenStream, V8MappingError> {
  let GeneratorState { result, .. } = &generator_state;

  let mut tokens = TokenStream::new();
  for arg in &generator_state.args {
    tokens.extend(quote!( #arg , ));
  }
  Ok(quote! {
    let #result = Self::call( #tokens );
  })
}

pub fn return_value(
  generator_state: &mut GeneratorState,
  ret_type: &RetVal,
) -> Result<TokenStream, V8MappingError> {
  match ret_type {
    RetVal::Infallible(ret_type) => {
      return_value_infallible(generator_state, ret_type)
    }
    RetVal::Result(ret_type) => return_value_result(generator_state, ret_type),
    _ => todo!(),
  }
}

pub fn return_value_infallible(
  generator_state: &mut GeneratorState,
  ret_type: &Arg,
) -> Result<TokenStream, V8MappingError> {
  let GeneratorState {
    deno_core,
    scope,
    result,
    retval,
    needs_retval,
    needs_scope,
    ..
  } = generator_state;

  let res = match ret_type {
    Arg::Void => {
      // TODO(mmastrac): revisit this. Ideally we wouldn't need to set
      // rv to null, but because of how serde_v8 works this is required
      // to keep compatibility with existing assumptions in `deno_core`
      // and `deno` itself.
      *needs_retval = true;
      quote! {#retval.set_null();}
    }
    Arg::Numeric(NumericArg::bool) => {
      *needs_retval = true;
      quote!(#retval.set_bool(#result as bool);)
    }
    Arg::Numeric(NumericArg::u8)
    | Arg::Numeric(NumericArg::u16)
    | Arg::Numeric(NumericArg::u32) => {
      *needs_retval = true;
      quote!(#retval.set_uint32(#result as u32);)
    }
    Arg::Numeric(NumericArg::i8)
    | Arg::Numeric(NumericArg::i16)
    | Arg::Numeric(NumericArg::i32) => {
      *needs_retval = true;
      quote!(#retval.set_int32(#result as i32);)
    }
    Arg::Numeric(NumericArg::f32 | NumericArg::f64) => {
      *needs_retval = true;
      quote!(#retval.set_double(#result as _);)
    }
    Arg::Special(Special::String) => {
      *needs_retval = true;
      *needs_scope = true;
      quote! {
        if #result.is_empty() {
          #retval.set_empty_string();
        } else {
          // This should not fail in normal cases
          // TODO(mmastrac): This has extra allocations that we need to get rid of, especially if the string
          // is ASCII. We could make an "external Rust String" string in V8 from these and re-use the allocation.
          let temp = #deno_core::v8::String::new(&mut #scope, &#result).unwrap();
          #retval.set(temp.into());
        }
      }
    }
    Arg::OptionNumeric(n) => {
      *needs_retval = true;
      *needs_scope = true;
      let some = return_value_infallible(generator_state, &Arg::Numeric(*n))?;
      gs_quote!(generator_state(result, retval) => {
        if let Some(#result) = #result {
          #some
        } else {
          #retval.set_null();
        }
      })
    }
    Arg::Option(Special::String) => {
      *needs_retval = true;
      *needs_scope = true;
      let some = return_value_infallible(
        generator_state,
        &Arg::Special(Special::String),
      )?;
      gs_quote!(generator_state(result, retval) => {
        if let Some(#result) = #result {
          #some
        } else {
          #retval.set_null();
        }
      })
    }
    Arg::OptionV8Local(_) => {
      *needs_retval = true;
      quote! {
        if let Some(#result) = #result {
          // We may have a non v8::Value here
          #retval.set(#result.into())
        } else {
          #retval.set_null();
        }
      }
    }
    Arg::V8Local(_) => {
      *needs_retval = true;
      quote! {
        // We may have a non v8::Value here
        #retval.set(#result.into())
      }
    }
    Arg::SerdeV8(_class) => {
      *needs_retval = true;
      *needs_scope = true;

      let deno_core = deno_core.clone();
      let scope = scope.clone();
      let result = result.clone();
      let retval = retval.clone();
      let err = format_ident!("{}_err", retval);
      let throw_exception = throw_type_error_string(generator_state, &err)?;

      quote! {
        let #result = match #deno_core::_ops::serde_rust_to_v8(&mut #scope, #result) {
          Ok(t) => t,
          Err(#err) => {
            #throw_exception
          }
        };
        #retval.set(#result.into())
      }
    }
    _ => {
      return Err(V8MappingError::NoMapping(
        "a slow return value",
        ret_type.clone(),
      ))
    }
  };

  Ok(res)
}

/// Puts a typed result into a [`v8::Value`].
pub fn return_value_v8_value(
  generator_state: &GeneratorState,
  ret_type: &Arg,
) -> Result<TokenStream, V8MappingError> {
  gs_extract!(generator_state(deno_core, scope, result));
  let res = match ret_type {
    Arg::Void => {
      quote!(Ok(#deno_core::v8::null(#scope).into()))
    }
    Arg::Numeric(NumericArg::bool) => {
      quote!(Ok(#deno_core::v8::Boolean::new(#result).into()))
    }
    Arg::Numeric(NumericArg::i8 | NumericArg::i16 | NumericArg::i32) => {
      quote!(Ok(#deno_core::v8::Integer::new(#scope, #result).into()))
    }
    Arg::Numeric(NumericArg::u8 | NumericArg::u16 | NumericArg::u32) => {
      quote!(Ok(#deno_core::v8::Integer::new_from_unsigned(#scope, #result).into()))
    }
    _ => {
      return Err(V8MappingError::NoMapping(
        "a v8 return value",
        ret_type.clone(),
      ))
    }
  };
  Ok(res)
}

pub fn return_value_result(
  generator_state: &mut GeneratorState,
  ret_type: &Arg,
) -> Result<TokenStream, V8MappingError> {
  let infallible = return_value_infallible(generator_state, ret_type)?;
  let exception = throw_exception(generator_state)?;

  let tokens = gs_quote!(generator_state(result) => (
    match #result {
      Ok(#result) => {
        #infallible
      }
      Err(err) => {
        #exception
      }
    };
  ));
  Ok(tokens)
}

/// Generates code to throw an exception, adding required additional dependencies as needed.
pub(crate) fn throw_exception(
  generator_state: &mut GeneratorState,
) -> Result<TokenStream, V8MappingError> {
  let maybe_scope = if generator_state.needs_scope {
    quote!()
  } else {
    with_scope(generator_state)
  };

  let maybe_opctx = if generator_state.needs_opctx {
    quote!()
  } else {
    with_opctx(generator_state)
  };

  let maybe_args = if generator_state.needs_args {
    quote!()
  } else {
    with_fn_args(generator_state)
  };

  Ok(gs_quote!(generator_state(deno_core, scope, opctx) => {
    #maybe_scope
    #maybe_args
    #maybe_opctx
    let err = err.into();
    let opstate = ::std::cell::RefCell::borrow(&*#opctx.state);
    let exception = #deno_core::error::to_v8_error(
      &mut #scope,
      opstate.get_error_class_fn,
      &err,
    );
    #scope.throw_exception(exception);
    return;
  }))
}

/// Generates code to throw an exception, adding required additional dependencies as needed.
fn throw_type_error(
  generator_state: &mut GeneratorState,
  message: String,
) -> Result<TokenStream, V8MappingError> {
  // Sanity check ASCII and a valid/reasonable message size
  debug_assert!(
    message.is_ascii()
      && message.len() < v8::String::max_length()
      && message.len() < 1024
  );

  let maybe_scope = if generator_state.needs_scope {
    quote!()
  } else {
    with_scope(generator_state)
  };

  Ok(gs_quote!(generator_state(deno_core, scope) => {
    #maybe_scope
    let msg = #deno_core::v8::String::new_from_one_byte(&mut #scope, #message.as_bytes(), #deno_core::v8::NewStringType::Normal).unwrap();
    let exc = #deno_core::v8::Exception::error(&mut #scope, msg);
    #scope.throw_exception(exc);
    return;
  }))
}

/// Generates code to throw an exception from a string variable, adding required additional dependencies as needed.
fn throw_type_error_string(
  generator_state: &mut GeneratorState,
  message: &Ident,
) -> Result<TokenStream, V8MappingError> {
  let maybe_scope = if generator_state.needs_scope {
    quote!()
  } else {
    with_scope(generator_state)
  };

  Ok(gs_quote!(generator_state(deno_core, scope) => {
    #maybe_scope
    // TODO(mmastrac): This might be allocating too much, even if it's on the error path
    let msg = #deno_core::v8::String::new(&mut #scope, &format!("{}", #deno_core::anyhow::Error::from(#message))).unwrap();
    let exc = #deno_core::v8::Exception::error(&mut #scope, msg);
    #scope.throw_exception(exc);
    return;
  }))
}

/// Generates code to throw an exception from a string variable, adding required additional dependencies as needed.
fn throw_type_error_static_string(
  generator_state: &mut GeneratorState,
  message: &Ident,
) -> Result<TokenStream, V8MappingError> {
  let maybe_scope = if generator_state.needs_scope {
    quote!()
  } else {
    with_scope(generator_state)
  };

  Ok(gs_quote!(generator_state(deno_core, scope) => {
    #maybe_scope
    let msg = #deno_core::v8::String::new_from_one_byte(&mut #scope, #message.as_bytes(), #deno_core::v8::NewStringType::Normal).unwrap();
    let exc = #deno_core::v8::Exception::error(&mut #scope, msg);
    #scope.throw_exception(exc);
    return;
  }))
}
