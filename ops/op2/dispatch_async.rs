use super::V8MappingError;
use super::V8SignatureMappingError;
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
use proc_macro2::TokenStream;
use quote::quote;

fn map_async_return_type(
  generator_state: &mut GeneratorState,
  ret_val: &RetVal,
) -> Result<(TokenStream, TokenStream, TokenStream), V8MappingError> {
  let return_value = match ret_val {
    RetVal::Future(r) | RetVal::ResultFuture(r) => {
      return_value_v8_value(generator_state, r)?
    }
    RetVal::FutureResult(r) | RetVal::ResultFutureResult(r) => {
      return_value_v8_value(generator_state, r)?
    }
    RetVal::Infallible(_) | RetVal::Result(_) => return Err("an async return"),
  };
  let (mapper, return_value_immediate) = match ret_val {
    RetVal::Future(r) | RetVal::ResultFuture(r) => (
      quote!(map_async_op_infallible),
      return_value_infallible(generator_state, r)?,
    ),
    RetVal::FutureResult(r) | RetVal::ResultFutureResult(r) => (
      quote!(map_async_op_fallible),
      return_value_result(generator_state, r)?,
    ),
    RetVal::Infallible(_) | RetVal::Result(_) => return Err("an async return"),
  };
  Ok((return_value, mapper, return_value_immediate))
}

pub(crate) fn generate_dispatch_async(
  config: &MacroConfig,
  generator_state: &mut GeneratorState,
  signature: &ParsedSignature,
) -> Result<TokenStream, V8SignatureMappingError> {
  let mut output = TokenStream::new();

  // Collect virtual arguments in a deferred list that we compute at the very end. This allows us to borrow
  // the scope/opstate in the intermediate stages.
  let mut args = TokenStream::new();
  let mut deferred = TokenStream::new();

  // Promise ID is the first arg
  let mut input_index = 1;

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

  // Always need context and args
  // TODO(mmastrac): Do we?
  generator_state.needs_opctx = true;
  generator_state.needs_args = true;
  generator_state.needs_scope = true;
  generator_state.needs_retval = true;

  let (return_value, mapper, return_value_immediate) =
    map_async_return_type(generator_state, &signature.ret_val).map_err(
      |s| {
        V8SignatureMappingError::NoRetValMapping(s, signature.ret_val.clone())
      },
    )?;

  args.extend(deferred);
  args.extend(call(generator_state));
  output.extend(gs_quote!(generator_state(deno_core, result, opctx) => {
    if #opctx.metrics_enabled() {
      #deno_core::_ops::dispatch_metrics_async(&#opctx, #deno_core::_ops::OpMetricsEvent::Dispatched);
    }
    let #result = {
      #args
    };
  }));

  if matches!(
    signature.ret_val,
    RetVal::ResultFuture(_) | RetVal::ResultFutureResult(_)
  ) {
    let exception = throw_exception(generator_state);
    output.extend(gs_quote!(generator_state(deno_core, opctx, result) => {
      let #result = match #result {
        Ok(#result) => #result,
        Err(err) => {
          if #opctx.metrics_enabled() {
            #deno_core::_ops::dispatch_metrics_async(&#opctx, #deno_core::_ops::OpMetricsEvent::Error);
          }
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
    gs_quote!(generator_state(deno_core, info, slow_function, slow_function_metrics) => {
      extern "C" fn #slow_function(#info: *const #deno_core::v8::FunctionCallbackInfo) {
        #with_scope
        #with_retval
        #with_args
        #with_opctx
        #with_opstate

        #output
      }

      extern "C" fn #slow_function_metrics(#info: *const #deno_core::v8::FunctionCallbackInfo) {
        Self::#slow_function(#info)
      }
    }),
  )
}
