// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use super::config::MacroConfig;
use super::dispatch_slow::call;
use super::dispatch_slow::extract_arg;
use super::dispatch_slow::from_arg;
use super::dispatch_slow::return_value_infallible;
use super::dispatch_slow::return_value_result;
use super::dispatch_slow::return_value_v8_value;
use super::dispatch_slow::throw_exception;
use super::dispatch_slow::with_fn_args;
use super::dispatch_slow::with_opctx;
use super::dispatch_slow::with_opstate;
use super::dispatch_slow::with_retval;
use super::dispatch_slow::with_scope;
use super::generator_state::gs_quote;
use super::generator_state::GeneratorState;
use super::signature::ParsedSignature;
use super::signature::RetVal;
use super::V8MappingError;
use proc_macro2::TokenStream;
use quote::quote;

pub(crate) fn generate_dispatch_async(
  config: &MacroConfig,
  generator_state: &mut GeneratorState,
  signature: &ParsedSignature,
) -> Result<TokenStream, V8MappingError> {
  let mut output = TokenStream::new();

  // Collect virtual arguments in a deferred list that we compute at the very end. This allows us to borrow
  // the scope/opstate in the intermediate stages.
  let mut args = TokenStream::new();
  let mut deferred = TokenStream::new();

  // Promise ID is the first arg
  let mut input_index = 1;

  for (index, arg) in signature.args.iter().enumerate() {
    if arg.is_virtual() {
      deferred.extend(from_arg(generator_state, index, arg)?);
    } else {
      args.extend(extract_arg(generator_state, index, input_index)?);
      args.extend(from_arg(generator_state, index, arg)?);
      input_index += 1;
    }
  }

  // Always need context and args
  // TODO(mmastrac): Do we?
  generator_state.needs_opctx = true;
  generator_state.needs_args = true;
  generator_state.needs_scope = true;
  generator_state.needs_retval = true;

  let return_value = match &signature.ret_val {
    RetVal::Future(r) | RetVal::ResultFuture(r) => {
      return_value_v8_value(generator_state, r)?
    }
    RetVal::FutureResult(r) | RetVal::ResultFutureResult(r) => {
      return_value_v8_value(generator_state, r)?
    }
    RetVal::Infallible(r) | RetVal::Result(r) => {
      return Err(V8MappingError::NoMapping("an async return", r.clone()))
    }
  };
  let (mapper, return_value_immediate) = match &signature.ret_val {
    RetVal::Future(r) | RetVal::ResultFuture(r) => (
      quote!(map_async_op_infallible),
      return_value_infallible(generator_state, r)?,
    ),
    RetVal::FutureResult(r) | RetVal::ResultFutureResult(r) => (
      quote!(map_async_op_fallible),
      return_value_result(generator_state, r)?,
    ),
    RetVal::Infallible(r) | RetVal::Result(r) => {
      return Err(V8MappingError::NoMapping("an async return", r.clone()))
    }
  };

  args.extend(deferred);
  args.extend(call(generator_state)?);
  output.extend(gs_quote!(generator_state(result) => {
    let #result = {
      #args
    };
  }));

  if matches!(
    signature.ret_val,
    RetVal::ResultFuture(_) | RetVal::ResultFutureResult(_)
  ) {
    let exception = throw_exception(generator_state)?;
    output.extend(gs_quote!(generator_state(result) => {
      let #result = match #result {
        Ok(#result) => #result,
        Err(err) => {
          // Handle eager error -- this will leave only a Future<R> or Future<Result<R>>
          #exception
        }
      };
    }));
  }

  let lazy = config.async_lazy;
  output.extend(gs_quote!(generator_state(promise_id, fn_args, result, opctx, scope, deno_core) => {
    let #promise_id = #deno_core::_ops::to_i32_option(&#fn_args.get(0)).unwrap_or_default();
    if let Some(#result) = #deno_core::_ops::#mapper(#opctx, #lazy, #promise_id, #result, |#scope, #result| {
      #return_value
    }) {
      // Eager poll returned a value
      #return_value_immediate
    }
  }));

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
    }}),
  )
}
