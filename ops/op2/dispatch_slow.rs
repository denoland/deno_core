// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use super::config::MacroConfig;
use super::dispatch_shared::v8_intermediate_to_arg;
use super::dispatch_shared::v8_intermediate_to_global_arg;
use super::dispatch_shared::v8_to_arg;
use super::dispatch_shared::v8slice_to_buffer;
use super::generator_state::gs_extract;
use super::generator_state::gs_quote;
use super::generator_state::GeneratorState;
use super::signature::Arg;
use super::signature::ArgMarker;
use super::signature::ArgSlowRetval;
use super::signature::BufferMode;
use super::signature::BufferSource;
use super::signature::BufferType;
use super::signature::External;
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
use syn::Type;

pub(crate) fn generate_dispatch_slow_call(
  generator_state: &mut GeneratorState,
  signature: &ParsedSignature,
  mut input_index: usize,
) -> Result<TokenStream, V8SignatureMappingError> {
  // Collect virtual arguments in a deferred list that we compute at the very end. This allows us to borrow
  // the scope/opstate in the intermediate stages.
  let mut args = TokenStream::new();
  let mut deferred = TokenStream::new();

  for (index, arg) in signature.args.iter().enumerate() {
    let arg_mapped = from_arg(generator_state, index, arg)
      .map_err(|s| V8SignatureMappingError::NoArgMapping(s, arg.clone()))?;
    if arg.is_virtual() {
      deferred.extend(arg_mapped);
    } else {
      args.extend(extract_arg(generator_state, index, input_index));
      args.extend(arg_mapped);
      input_index += 1;
    }
  }

  args.extend(deferred);
  args.extend(call(generator_state));
  Ok(args)
}

pub(crate) fn generate_dispatch_slow(
  config: &MacroConfig,
  generator_state: &mut GeneratorState,
  signature: &ParsedSignature,
) -> Result<TokenStream, V8SignatureMappingError> {
  let mut output = TokenStream::new();

  // Fast ops require the slow op to check op_ctx for the last error
  if config.fast && matches!(signature.ret_val, RetVal::Result(_)) {
    generator_state.needs_opctx = true;
    let throw_exception = throw_exception(generator_state);
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

  let args = generate_dispatch_slow_call(generator_state, signature, 0)?;

  output.extend(gs_quote!(generator_state(result) => (let #result = {
    #args
  };)));
  output.extend(return_value(generator_state, &signature.ret_val).map_err(
    |s| V8SignatureMappingError::NoRetValMapping(s, signature.ret_val.clone()),
  )?);

  // We only generate the isolate if we need it but don't need a scope. We call it `scope`.
  let with_isolate =
    if generator_state.needs_isolate && !generator_state.needs_scope {
      with_isolate(generator_state)
    } else {
      quote!()
    };

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

  let with_js_runtime_state = if generator_state.needs_js_runtime_state {
    with_js_runtime_state(generator_state)
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
    gs_quote!(generator_state(deno_core, opctx, info, slow_function, slow_function_metrics) => {
      extern "C" fn #slow_function(#info: *const #deno_core::v8::FunctionCallbackInfo) {
        #with_scope
        #with_retval
        #with_args
        #with_opctx
        #with_isolate
        #with_opstate
        #with_js_runtime_state

        #output
      }
      extern "C" fn #slow_function_metrics(#info: *const #deno_core::v8::FunctionCallbackInfo) {
        let args = #deno_core::v8::FunctionCallbackArguments::from_function_callback_info(unsafe {
          &*info
        });
        let #opctx = unsafe {
            &*(#deno_core::v8::Local::<#deno_core::v8::External>::cast(args.data()).value()
                as *const #deno_core::_ops::OpCtx)
        };

        #deno_core::_ops::dispatch_metrics_slow(&#opctx, #deno_core::_ops::OpMetricsEvent::Dispatched);
        Self::#slow_function(#info);
        #deno_core::_ops::dispatch_metrics_slow(&#opctx, #deno_core::_ops::OpMetricsEvent::Completed);
      }
    }),
  )
}

pub(crate) fn with_isolate(
  generator_state: &mut GeneratorState,
) -> TokenStream {
  generator_state.needs_opctx = true;
  gs_quote!(generator_state(opctx, scope) =>
    (let mut #scope = unsafe { &mut *#opctx.isolate };)
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

pub(crate) fn with_js_runtime_state(
  generator_state: &mut GeneratorState,
) -> TokenStream {
  generator_state.needs_opctx = true;
  gs_quote!(generator_state(opctx, js_runtime_state) =>
    (let #js_runtime_state = std::rc::Weak::upgrade(&#opctx.runtime_state).unwrap();)
  )
}

pub fn extract_arg(
  generator_state: &mut GeneratorState,
  index: usize,
  input_index: usize,
) -> TokenStream {
  let GeneratorState { fn_args, .. } = &generator_state;
  let arg_ident = generator_state.args.get(index);

  quote!(
    let #arg_ident = #fn_args.get(#input_index as i32);
  )
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
    opctx,
    js_runtime_state,
    needs_scope,
    needs_isolate,
    needs_opstate,
    needs_opctx,
    needs_js_runtime_state,
    ..
  } = &mut generator_state;
  let arg_ident = args
    .get(index)
    .expect("Argument at index was missing")
    .clone();
  let arg_temp = format_ident!("{}_temp", arg_ident);
  let res = match arg {
    Arg::Numeric(NumericArg::bool, _) => quote! {
      let #arg_ident = #arg_ident.is_true();
    },
    Arg::Numeric(NumericArg::u8, _)
    | Arg::Numeric(NumericArg::u16, _)
    | Arg::Numeric(NumericArg::u32, _) => {
      from_arg_option(generator_state, &arg_ident, "u32")?
    }
    Arg::Numeric(NumericArg::i8, _)
    | Arg::Numeric(NumericArg::i16, _)
    | Arg::Numeric(NumericArg::i32, _)
    | Arg::Numeric(NumericArg::__SMI__, _) => {
      from_arg_option(generator_state, &arg_ident, "i32")?
    }
    Arg::Numeric(NumericArg::u64 | NumericArg::usize, NumericFlag::None) => {
      from_arg_option(generator_state, &arg_ident, "u64")?
    }
    Arg::Numeric(NumericArg::i64 | NumericArg::isize, NumericFlag::None) => {
      from_arg_option(generator_state, &arg_ident, "i64")?
    }
    Arg::Numeric(
      NumericArg::u64 | NumericArg::usize | NumericArg::i64 | NumericArg::isize,
      NumericFlag::Number,
    ) => from_arg_option(generator_state, &arg_ident, "f64")?,
    Arg::Numeric(NumericArg::f32, _) => {
      from_arg_option(generator_state, &arg_ident, "f32")?
    }
    Arg::Numeric(NumericArg::f64, _) => {
      from_arg_option(generator_state, &arg_ident, "f64")?
    }
    Arg::OptionNumeric(numeric, flag) => {
      let some =
        from_arg(generator_state, index, &Arg::Numeric(*numeric, *flag))?;
      quote! {
        let #arg_ident = if #arg_ident.is_null_or_undefined() {
          None
        } else {
          #some
          Some(#arg_ident)
        };
      }
    }
    Arg::OptionString(Strings::String) => {
      // Only requires isolate, not a full scope
      *needs_isolate = true;
      quote! {
        let #arg_ident = if #arg_ident.is_null_or_undefined() {
          None
        } else {
          Some(#deno_core::_ops::to_string(&mut #scope, &#arg_ident))
        };
      }
    }
    Arg::String(Strings::String) => {
      // Only requires isolate, not a full scope
      *needs_isolate = true;
      quote! {
        let #arg_ident = #deno_core::_ops::to_string(&mut #scope, &#arg_ident);
      }
    }
    Arg::String(Strings::RefStr) => {
      // Only requires isolate, not a full scope
      *needs_isolate = true;
      quote! {
        // Trade stack space for potentially non-allocating strings
        let mut #arg_temp: [::std::mem::MaybeUninit<u8>; #deno_core::_ops::STRING_STACK_BUFFER_SIZE] = [::std::mem::MaybeUninit::uninit(); #deno_core::_ops::STRING_STACK_BUFFER_SIZE];
        let #arg_ident = &#deno_core::_ops::to_str(&mut #scope, &#arg_ident, &mut #arg_temp);
      }
    }
    Arg::String(Strings::CowStr) => {
      // Only requires isolate, not a full scope
      *needs_isolate = true;
      quote! {
        // Trade stack space for potentially non-allocating strings
        let mut #arg_temp: [::std::mem::MaybeUninit<u8>; #deno_core::_ops::STRING_STACK_BUFFER_SIZE] = [::std::mem::MaybeUninit::uninit(); #deno_core::_ops::STRING_STACK_BUFFER_SIZE];
        let #arg_ident = #deno_core::_ops::to_str(&mut #scope, &#arg_ident, &mut #arg_temp);
      }
    }
    Arg::String(Strings::CowByte) => {
      // Only requires isolate, not a full scope
      *needs_isolate = true;
      let throw_exception =
        throw_type_error_static_string(generator_state, &arg_ident)?;
      gs_quote!(generator_state(deno_core, scope) => {
        // Trade stack space for potentially non-allocating strings
        let #arg_ident = match #deno_core::_ops::to_cow_one_byte(&mut #scope, &#arg_ident) {
          Ok(#arg_ident) => #arg_ident,
          Err(#arg_ident) => {
            #throw_exception
          }
        };
      })
    }
    Arg::Buffer(buffer_type, mode, source) => {
      // Explicit temporary lifetime extension so we can take a reference
      let temp = format_ident!("{}_temp", arg_ident);
      let buffer = from_arg_array_or_buffer(
        generator_state,
        &arg_ident,
        *buffer_type,
        *mode,
        *source,
        &temp,
      )?;
      quote! {
        let mut #temp;
        #buffer
      }
    }
    Arg::OptionBuffer(buffer_type, mode, source) => {
      // Explicit temporary lifetime extension so we can take a reference
      let temp = format_ident!("{}_temp", arg_ident);
      let some = from_arg_array_or_buffer(
        generator_state,
        &arg_ident,
        *buffer_type,
        *mode,
        *source,
        &temp,
      )?;
      quote! {
        let mut #temp;
        let #arg_ident = if #arg_ident.is_null_or_undefined() {
          None
        } else {
          #some
          Some(#arg_ident)
        };
      }
    }
    Arg::External(External::Ptr(_)) => {
      from_arg_option(generator_state, &arg_ident, "external")?
    }
    Arg::Special(Special::Isolate) => {
      *needs_opctx = true;
      quote!(let #arg_ident = #opctx.isolate;)
    }
    Arg::Ref(_, Special::HandleScope) => {
      *needs_scope = true;
      quote!(let #arg_ident = &mut #scope;)
    }
    Arg::Ref(RefType::Ref, Special::OpState) => {
      *needs_opstate = true;
      quote!(let #arg_ident = &::std::cell::RefCell::borrow(&#opstate);)
    }
    Arg::Ref(RefType::Mut, Special::OpState) => {
      *needs_opstate = true;
      quote!(let #arg_ident = &mut ::std::cell::RefCell::borrow_mut(&#opstate);)
    }
    Arg::RcRefCell(Special::OpState) => {
      *needs_opstate = true;
      quote!(let #arg_ident = #opstate.clone();)
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
      *needs_opstate = true;
      let state =
        syn::parse_str::<Type>(state).expect("Failed to reparse state type");
      quote! {
        let #arg_ident = ::std::cell::RefCell::borrow(&#opstate);
        let #arg_ident = #deno_core::_ops::opstate_borrow::<#state>(&#arg_ident);
      }
    }
    Arg::State(RefType::Mut, state) => {
      *needs_opstate = true;
      let state =
        syn::parse_str::<Type>(state).expect("Failed to reparse state type");
      quote! {
        let mut #arg_ident = ::std::cell::RefCell::borrow_mut(&#opstate);
        let #arg_ident = #deno_core::_ops::opstate_borrow_mut::<#state>(&mut #arg_ident);
      }
    }
    Arg::OptionState(RefType::Ref, state) => {
      *needs_opstate = true;
      let state =
        syn::parse_str::<Type>(state).expect("Failed to reparse state type");
      quote! {
        let #arg_ident = &::std::cell::RefCell::borrow(&#opstate);
        let #arg_ident = #arg_ident.try_borrow::<#state>();
      }
    }
    Arg::OptionState(RefType::Mut, state) => {
      *needs_opstate = true;
      let state =
        syn::parse_str::<Type>(state).expect("Failed to reparse state type");
      quote! {
        let mut #arg_ident = &mut ::std::cell::RefCell::borrow_mut(&#opstate);
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
      // Only requires isolate, not a full scope
      *needs_isolate = true;
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
    _ => return Err("a slow argument"),
  };
  Ok(res)
}

/// Converts an argument using a simple `to_XXX_option`-style method.
pub fn from_arg_option(
  generator_state: &mut GeneratorState,
  arg_ident: &Ident,
  numeric: &str,
) -> Result<TokenStream, V8MappingError> {
  let exception =
    throw_type_error(generator_state, format!("expected {numeric}"))?;
  let convert = format_ident!("to_{numeric}_option");
  Ok(gs_quote!(generator_state(deno_core) => (
    let Some(#arg_ident) = #deno_core::_ops::#convert(&#arg_ident) else {
      #exception
    };
    let #arg_ident = #arg_ident as _;
  )))
}

pub fn from_arg_array_or_buffer(
  generator_state: &mut GeneratorState,
  arg_ident: &Ident,
  buffer_type: BufferType,
  buffer_mode: BufferMode,
  buffer_source: BufferSource,
  temp: &Ident,
) -> Result<TokenStream, V8MappingError> {
  match buffer_source {
    BufferSource::TypedArray => from_arg_buffer(
      generator_state,
      arg_ident,
      buffer_type,
      buffer_mode,
      temp,
    ),
    BufferSource::ArrayBuffer => from_arg_arraybuffer(
      generator_state,
      arg_ident,
      buffer_type,
      buffer_mode,
      temp,
    ),
    BufferSource::Any => from_arg_any_buffer(
      generator_state,
      arg_ident,
      buffer_type,
      buffer_mode,
      temp,
    ),
  }
}

pub fn from_arg_buffer(
  generator_state: &mut GeneratorState,
  arg_ident: &Ident,
  buffer_type: BufferType,
  buffer_mode: BufferMode,
  temp: &Ident,
) -> Result<TokenStream, V8MappingError> {
  let err = format_ident!("{}_err", arg_ident);
  let throw_exception = throw_type_error_static_string(generator_state, &err)?;

  let array = buffer_type.element();

  let to_v8_slice = if matches!(buffer_mode, BufferMode::Detach) {
    generator_state.needs_scope = true;
    gs_quote!(generator_state(deno_core, scope) => { #deno_core::_ops::to_v8_slice_detachable::<#array>(&mut #scope, #arg_ident) })
  } else {
    gs_quote!(generator_state(deno_core) => { #deno_core::_ops::to_v8_slice::<#array>(#arg_ident) })
  };

  let make_v8slice = quote!(
    #temp = match unsafe { #to_v8_slice } {
      Ok(#arg_ident) => #arg_ident,
      Err(#err) => {
        #throw_exception
      }
    };
  );

  let make_arg = v8slice_to_buffer(
    &generator_state.deno_core,
    arg_ident,
    temp,
    buffer_type,
  )?;

  Ok(quote! {
    #make_v8slice
    #make_arg
  })
}

pub fn from_arg_arraybuffer(
  generator_state: &mut GeneratorState,
  arg_ident: &Ident,
  buffer_type: BufferType,
  buffer_mode: BufferMode,
  temp: &Ident,
) -> Result<TokenStream, V8MappingError> {
  let err = format_ident!("{}_err", arg_ident);
  let throw_exception = throw_type_error_static_string(generator_state, &err)?;

  let to_v8_slice = if matches!(buffer_mode, BufferMode::Detach) {
    quote!(to_v8_slice_buffer_detachable)
  } else {
    quote!(to_v8_slice_buffer)
  };

  let make_v8slice = gs_quote!(generator_state(deno_core) => {
    #temp = match unsafe { #deno_core::_ops::#to_v8_slice(#arg_ident) } {
      Ok(#arg_ident) => #arg_ident,
      Err(#err) => {
        #throw_exception
      }
    };
  });

  let make_arg = v8slice_to_buffer(
    &generator_state.deno_core,
    arg_ident,
    temp,
    buffer_type,
  )?;

  Ok(quote! {
    #make_v8slice
    #make_arg
  })
}

pub fn from_arg_any_buffer(
  generator_state: &mut GeneratorState,
  arg_ident: &Ident,
  buffer_type: BufferType,
  buffer_mode: BufferMode,
  temp: &Ident,
) -> Result<TokenStream, V8MappingError> {
  let err = format_ident!("{}_err", arg_ident);
  let throw_exception = throw_type_error_static_string(generator_state, &err)?;

  let to_v8_slice = if matches!(buffer_mode, BufferMode::Detach) {
    quote!(to_v8_slice_any_detachable)
  } else {
    quote!(to_v8_slice_any)
  };

  let make_v8slice = gs_quote!(generator_state(deno_core) => {
    #temp = match unsafe { #deno_core::_ops::#to_v8_slice(#arg_ident) } {
      Ok(#arg_ident) => #arg_ident,
      Err(#err) => {
        #throw_exception
      }
    };
  });

  let make_arg = v8slice_to_buffer(
    &generator_state.deno_core,
    arg_ident,
    temp,
    buffer_type,
  )?;

  Ok(quote! {
    #make_v8slice
    #make_arg
  })
}

pub fn call(generator_state: &mut GeneratorState) -> TokenStream {
  let mut tokens = TokenStream::new();
  for arg in &generator_state.args {
    tokens.extend(quote!( #arg , ));
  }
  quote!(Self::call( #tokens ))
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
  // In the future we may be able to make this false for void again
  generator_state.needs_retval = true;

  let result = match ret_type.marker() {
    ArgMarker::ArrayBuffer => {
      gs_quote!(generator_state(deno_core, result) => (#deno_core::_ops::RustToV8Marker::<#deno_core::_ops::ArrayBufferMarker, _>::from(#result)))
    }
    ArgMarker::Serde => {
      gs_quote!(generator_state(deno_core, result) => (#deno_core::_ops::RustToV8Marker::<#deno_core::_ops::SerdeMarker, _>::from(#result)))
    }
    ArgMarker::Smi => {
      gs_quote!(generator_state(deno_core, result) => (#deno_core::_ops::RustToV8Marker::<#deno_core::_ops::SmiMarker, _>::from(#result)))
    }
    ArgMarker::Number => {
      gs_quote!(generator_state(deno_core, result) => (#deno_core::_ops::RustToV8Marker::<#deno_core::_ops::NumberMarker, _>::from(#result)))
    }
    ArgMarker::None => gs_quote!(generator_state(result) => (#result)),
  };
  let res = match ret_type.slow_retval() {
    ArgSlowRetval::RetVal => {
      gs_quote!(generator_state(deno_core, retval) => (#deno_core::_ops::RustToV8RetVal::to_v8_rv(#result, &mut #retval)))
    }
    ArgSlowRetval::RetValFallible => {
      generator_state.needs_scope = true;
      let err = format_ident!("{}_err", generator_state.retval);
      let throw_exception = throw_type_error_string(generator_state, &err)?;

      gs_quote!(generator_state(deno_core, scope, retval) => (match #deno_core::_ops::RustToV8Fallible::to_v8_fallible(#result, &mut #scope) {
        Ok(v) => #retval.set(v),
        Err(#err) => {
          #throw_exception
        },
      }))
    }
    ArgSlowRetval::V8Local => {
      generator_state.needs_scope = true;
      gs_quote!(generator_state(deno_core, scope, retval) => (#retval.set(#deno_core::_ops::RustToV8::to_v8(#result, &mut #scope))))
    }
    ArgSlowRetval::V8LocalNoScope => {
      gs_quote!(generator_state(deno_core, retval) => (#retval.set(#deno_core::_ops::RustToV8NoScope::to_v8(#result))))
    }
    ArgSlowRetval::V8LocalFalliable => {
      generator_state.needs_scope = true;
      let err = format_ident!("{}_err", generator_state.retval);
      let throw_exception = throw_type_error_string(generator_state, &err)?;

      gs_quote!(generator_state(deno_core, scope, retval) => (match #deno_core::_ops::RustToV8Fallible::to_v8_fallible(#result, &mut #scope) {
        Ok(v) => #retval.set(v),
        Err(#err) => {
          #throw_exception
        },
      }))
    }
    ArgSlowRetval::None => return Err("a slow return value"),
  };

  Ok(res)
}

/// Puts a typed result into a [`v8::Value`].
pub fn return_value_v8_value(
  generator_state: &GeneratorState,
  ret_type: &Arg,
) -> Result<TokenStream, V8MappingError> {
  gs_extract!(generator_state(deno_core, scope, result));
  let result = match ret_type.marker() {
    ArgMarker::ArrayBuffer => {
      quote!(#deno_core::_ops::RustToV8Marker::<#deno_core::_ops::ArrayBufferMarker, _>::from(#result))
    }
    ArgMarker::Serde => {
      quote!(#deno_core::_ops::RustToV8Marker::<#deno_core::_ops::SerdeMarker, _>::from(#result))
    }
    ArgMarker::Smi => {
      quote!(#deno_core::_ops::RustToV8Marker::<#deno_core::_ops::SmiMarker, _>::from(#result))
    }
    ArgMarker::Number => {
      quote!(#deno_core::_ops::RustToV8Marker::<#deno_core::_ops::NumberMarker, _>::from(#result))
    }
    ArgMarker::None => quote!(#result),
  };
  let res = match ret_type.slow_retval() {
    ArgSlowRetval::RetVal | ArgSlowRetval::V8Local => {
      quote!(Ok(#deno_core::_ops::RustToV8::to_v8(#result, #scope)))
    }
    ArgSlowRetval::V8LocalNoScope => {
      quote!(Ok(#deno_core::_ops::RustToV8NoScope::to_v8(#result)))
    }
    ArgSlowRetval::RetValFallible | ArgSlowRetval::V8LocalFalliable => {
      quote!(#deno_core::_ops::RustToV8Fallible::to_v8_fallible(#result, #scope))
    }
    ArgSlowRetval::None => return Err("a v8 return value"),
  };
  Ok(res)
}

pub fn return_value_result(
  generator_state: &mut GeneratorState,
  ret_type: &Arg,
) -> Result<TokenStream, V8MappingError> {
  let infallible = return_value_infallible(generator_state, ret_type)?;
  let exception = throw_exception(generator_state);

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
) -> TokenStream {
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

  gs_quote!(generator_state(deno_core, scope, opctx) => {
    #maybe_scope
    #maybe_args
    #maybe_opctx
    let err = err.into();
    let exception = #deno_core::error::to_v8_error(
      &mut #scope,
      #opctx.get_error_class_fn,
      &err,
    );
    #scope.throw_exception(exception);
    return;
  })
}

/// Generates code to throw an exception, adding required additional dependencies as needed.
fn throw_type_error(
  generator_state: &mut GeneratorState,
  message: String,
) -> Result<TokenStream, V8MappingError> {
  // Sanity check ASCII and a valid/reasonable message size
  debug_assert!(message.is_ascii() && message.len() < 1024);

  let maybe_scope = if generator_state.needs_scope {
    quote!()
  } else {
    with_scope(generator_state)
  };

  Ok(gs_quote!(generator_state(deno_core, scope) => {
    #maybe_scope
    let msg = #deno_core::v8::String::new_from_one_byte(&mut #scope, #message.as_bytes(), #deno_core::v8::NewStringType::Normal).unwrap();
    let exc = #deno_core::v8::Exception::type_error(&mut #scope, msg);
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
    let exc = #deno_core::v8::Exception::type_error(&mut #scope, msg);
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
    let exc = #deno_core::v8::Exception::type_error(&mut #scope, msg);
    #scope.throw_exception(exc);
    return;
  }))
}
