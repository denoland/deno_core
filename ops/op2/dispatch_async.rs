use proc_macro2::TokenStream;
use quote::quote;

use super::dispatch_slow::call;
use super::dispatch_slow::extract_arg;
use super::dispatch_slow::from_arg;
use super::dispatch_slow::return_value;
use super::dispatch_slow::return_value_infallible;
use super::dispatch_slow::return_value_result;
use super::dispatch_slow::return_value_v8_value;
use super::dispatch_slow::with_fn_args;
use super::dispatch_slow::with_opctx;
use super::dispatch_slow::with_opstate;
use super::dispatch_slow::with_retval;
use super::dispatch_slow::with_scope;
use super::generator_state::GeneratorState;
use super::signature::ParsedSignature;
use super::signature::RetVal;
use super::MacroConfig;
use super::V8MappingError;

pub(crate) fn generate_dispatch_async(
  config: &MacroConfig,
  generator_state: &mut GeneratorState,
  signature: &ParsedSignature,
) -> Result<TokenStream, V8MappingError> {
  let mut output = TokenStream::new();

  // Collect virtual arguments in a deferred list that we compute at the very end. This allows us to borrow
  // the scope/opstate in the intermediate stages.
  let mut deferred = TokenStream::new();

  // Promise ID is the first arg
  let mut input_index = 1;

  for (index, arg) in signature.args.iter().enumerate() {
    if arg.is_virtual() {
      deferred.extend(from_arg(generator_state, index, arg)?);
    } else {
      output.extend(extract_arg(generator_state, index, input_index)?);
      output.extend(from_arg(generator_state, index, arg)?);
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
    RetVal::Future(r) | RetVal::ResultFuture(r) => return_value_v8_value(
      &generator_state.deno_core,
      &generator_state.scope,
      &generator_state.result,
      r,
    )?,
    RetVal::FutureResult(r) | RetVal::ResultFutureResult(r) => {
      return_value_v8_value(
        &generator_state.deno_core,
        &generator_state.scope,
        &generator_state.result,
        r,
      )?
    }
    RetVal::Infallible(r) | RetVal::Result(r) => {
      return Err(V8MappingError::NoMapping("an async return", r.clone()))
    }
  };
  let return_value_immediate = match &signature.ret_val {
    RetVal::Future(r) | RetVal::ResultFuture(r) => {
      return_value_infallible(generator_state, r)?
    }
    RetVal::FutureResult(r) | RetVal::ResultFutureResult(r) => {
      return_value_result(generator_state, r)?
    }
    RetVal::Infallible(r) | RetVal::Result(r) => {
      return Err(V8MappingError::NoMapping("an async return", r.clone()))
    }
  };

  output.extend(deferred);
  output.extend(call(generator_state)?);
  let retval = &generator_state.retval;
  let result = &generator_state.result;
  let opctx = &generator_state.opctx;
  let scope = &generator_state.scope;
  let deno_core = &generator_state.deno_core;

  if matches!(
    signature.ret_val,
    RetVal::ResultFuture(_) | RetVal::ResultFutureResult(_)
  ) {
    output.extend(quote! {
      let Ok(#result) = #result else {
        // Handle eager error -- this will leave only a Future<R> or Future<Result<R>>
        // ...
      };
    });
  }

  output.extend(quote! {
    // TODO(mmastrac): We are extending the eager polling behaviour temporarily to op2, but I would like to get
    // rid of it as soon as we can
    if let Some(#result) = #deno_core::_ops::map_async_op_infallible(#opctx, 0, #result, |#scope, #result| {
      #return_value
    }) {
      // Eager poll returned a value
      #return_value_immediate
    }
  });

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

  let GeneratorState {
    deno_core,
    info,
    slow_function,
    ..
  } = &generator_state;

  Ok(quote! {
    extern "C" fn #slow_function(#info: *const #deno_core::v8::FunctionCallbackInfo) {
    #with_scope
    #with_retval
    #with_args
    #with_opctx
    #with_opstate

    #output
  }})
}
