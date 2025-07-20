// Copyright 2018-2025 the Deno authors. MIT license.

use super::V8MappingError;
use super::V8SignatureMappingError;
use super::config::MacroConfig;
use super::dispatch_slow::generate_dispatch_slow_call;
use super::dispatch_slow::return_value_v8_value;
use super::dispatch_slow::throw_exception;
use super::dispatch_slow::with_fn_args;
use super::dispatch_slow::with_opctx;
use super::dispatch_slow::with_opstate;
use super::dispatch_slow::with_retval;
use super::dispatch_slow::with_scope;
use super::dispatch_slow::with_self;
use super::dispatch_slow::with_stack_trace;
use super::generator_state::GeneratorState;
use super::generator_state::gs_quote;
use super::signature::RetVal;
use super::signature::{Arg, ArgMarker, ArgSlowRetval, ParsedSignature};
use proc_macro2::TokenStream;
use quote::{format_ident, quote};

pub fn resolve_value_infallible(
  generator_state: &mut GeneratorState,
  ret_type: &Arg,
) -> Result<TokenStream, V8MappingError> {
  // In the future we may be able to make this false for void again
  generator_state.needs_retval = true;

  let result = match ret_type.marker() {
    ArgMarker::ArrayBuffer => {
      gs_quote!(generator_state(result) => (deno_core::_ops::RustToV8Marker::<deno_core::_ops::ArrayBufferMarker, _>::from(#result)))
    }
    ArgMarker::Serde => {
      gs_quote!(generator_state(result) => (deno_core::_ops::RustToV8Marker::<deno_core::_ops::SerdeMarker, _>::from(#result)))
    }
    ArgMarker::Smi => {
      gs_quote!(generator_state(result) => (deno_core::_ops::RustToV8Marker::<deno_core::_ops::SmiMarker, _>::from(#result)))
    }
    ArgMarker::Number => {
      gs_quote!(generator_state(result) => (deno_core::_ops::RustToV8Marker::<deno_core::_ops::NumberMarker, _>::from(#result)))
    }
    ArgMarker::Cppgc if generator_state.use_this_cppgc => {
      generator_state.needs_isolate = true;
      let wrap_object = match ret_type {
        Arg::CppGcProtochain(chain) => {
          let wrap_object = format_ident!("wrap_object{}", chain.len());
          quote!(#wrap_object)
        }
        _ => {
          if generator_state.use_proto_cppgc {
            quote!(wrap_object1)
          } else {
            quote!(wrap_object)
          }
        }
      };
      gs_quote!(generator_state(result, scope) => (
           Some(deno_core::cppgc::#wrap_object(&mut #scope, args.this(), #result))
      ))
    }
    ArgMarker::Cppgc if generator_state.use_proto_cppgc => {
      let marker = quote!(deno_core::_ops::RustToV8Marker::<deno_core::_ops::CppGcProtoMarker, _>::from);
      if ret_type.is_option() {
        gs_quote!(generator_state(result) => (#result.map(#marker)))
      } else {
        gs_quote!(generator_state(result) => (#marker(#result)))
      }
    }
    ArgMarker::Cppgc => {
      let marker = quote!(deno_core::_ops::RustToV8Marker::<deno_core::_ops::CppGcMarker, _>::from);
      if ret_type.is_option() {
        gs_quote!(generator_state(result) => (#result.map(#marker)))
      } else {
        gs_quote!(generator_state(result) => (#marker(#result)))
      }
    }
    ArgMarker::ToV8 => {
      gs_quote!(generator_state(result) => (deno_core::_ops::RustToV8Marker::<deno_core::_ops::ToV8Marker, _>::from(#result)))
    }
    ArgMarker::Undefined => {
      gs_quote!(generator_state(scope) => (deno_core::v8::undefined(&mut #scope)))
    }
    ArgMarker::None => {
      gs_quote!(generator_state(result) => (#result))
    }
  };
  generator_state.needs_scope = true;
  generator_state.needs_opctx = true;
  let res = match ret_type.slow_retval() {
    ArgSlowRetval::RetVal => {
      gs_quote!(generator_state(scope, opctx, promise_id) => {
        let value = deno_core::_ops::RustToV8::to_v8(#result, &mut #scope);
        #opctx.resolve_promise(&mut #scope, #promise_id, value);
      })
    }
    ArgSlowRetval::RetValFallible => {
      let err = format_ident!("{}_err", generator_state.retval);

      gs_quote!(generator_state(scope, opctx, promise_id) => (match deno_core::_ops::RustToV8Fallible::to_v8_fallible(#result, &mut #scope) {
        Ok(v) => #opctx.resolve_promise(&mut #scope, #promise_id, v),
        Err(#err) => {
          let exception = deno_core::error::to_v8_error(
            &mut #scope,
            &#err,
          );
          #opctx.reject_promise(&mut #scope, #promise_id, exception)
        },
      }))
    }
    ArgSlowRetval::V8Local => {
      gs_quote!(generator_state(scope, opctx, promise_id) => {
        let value = deno_core::_ops::RustToV8::to_v8(#result, &mut #scope);
        #opctx.resolve_promise(&mut #scope, #promise_id, value);
      })
    }
    ArgSlowRetval::V8LocalNoScope => {
      gs_quote!(generator_state(scope, opctx, promise_id) => {
        let value = deno_core::_ops::RustToV8NoScope::to_v8(#result);
        #opctx.resolve_promise(&mut #scope, #promise_id, value);
      })
    }
    ArgSlowRetval::V8LocalFalliable => {
      let err = format_ident!("{}_err", generator_state.retval);

      gs_quote!(generator_state(scope, opctx, promise_id) => (match deno_core::_ops::RustToV8Fallible::to_v8_fallible(#result, &mut #scope) {
        Ok(v) => #opctx.resolve_promise(&mut #scope, #promise_id, v),
        Err(#err) => {
          let exception = deno_core::error::to_v8_error(
            &mut #scope,
            &#err,
          );
          #opctx.reject_promise(&mut #scope, #promise_id, exception)
        },
      }))
    }
    ArgSlowRetval::None => return Err("a slow return value"),
  };

  Ok(res)
}

/// Generates code to reject an error, adding required additional dependencies as needed.
pub(crate) fn reject_error(
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

  gs_quote!(generator_state(scope, opctx, promise_id) => {
    #maybe_scope
    #maybe_args
    #maybe_opctx
    let exception = deno_core::error::to_v8_error(
      &mut #scope,
      &err,
    );
    #opctx.reject_promise(&mut #scope, #promise_id, exception);
    return 1;
  })
}

pub fn resolve_value_result(
  generator_state: &mut GeneratorState,
  ret_type: &Arg,
) -> Result<TokenStream, V8MappingError> {
  let infallible = resolve_value_infallible(generator_state, ret_type)?;
  let exception = reject_error(generator_state);

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

pub(crate) fn map_async_return_type(
  generator_state: &mut GeneratorState,
  ret_val: &RetVal,
) -> Result<(TokenStream, TokenStream, TokenStream), V8MappingError> {
  let return_value = return_value_v8_value(generator_state, ret_val.arg())?;
  let (mapper, return_value_immediate) = match ret_val {
    RetVal::Infallible(r, true)
    | RetVal::Future(r)
    | RetVal::ResultFuture(r) => (quote!(map_async_op_infallible), {
      resolve_value_infallible(generator_state, r)?
    }),
    RetVal::Result(r, true)
    | RetVal::FutureResult(r)
    | RetVal::ResultFutureResult(r) => (quote!(map_async_op_fallible), {
      resolve_value_result(generator_state, r)?
    }),
    RetVal::Infallible(_, false) | RetVal::Result(_, false) => {
      return Err("an async return");
    }
  };
  Ok((return_value, mapper, return_value_immediate))
}

pub(crate) fn generate_dispatch_async(
  config: &MacroConfig,
  generator_state: &mut GeneratorState,
  signature: &ParsedSignature,
) -> Result<TokenStream, V8SignatureMappingError> {
  let mut output = TokenStream::new();

  let with_self = if generator_state.needs_self {
    with_self(generator_state, &signature.ret_val)
  } else {
    quote!()
  };

  generator_state.needs_scope = true;
  let args = generate_dispatch_slow_call(generator_state, signature, 0)?;

  // Always need context and args
  generator_state.needs_opctx = true;
  generator_state.needs_args = true;

  // We don't have an isolate-only fast path for async yet
  generator_state.needs_scope |= generator_state.needs_isolate;

  let (return_value, mapper, return_value_immediate) =
    map_async_return_type(generator_state, &signature.ret_val).map_err(
      |s| {
        V8SignatureMappingError::NoRetValMapping(s, signature.ret_val.clone())
      },
    )?;

  output.extend(gs_quote!(generator_state(result) => {
    let #result = {
      #args
    };
  }));

  // TODO(mmastrac): we should save this unwrapped result
  if signature.ret_val.unwrap_result().is_some() {
    let exception = throw_exception(generator_state);
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

  output.extend(
    gs_quote!(generator_state(retval, opctx, scope, promise_id) => {
      let #promise_id = #opctx.create_promise(&mut #scope);
      #retval.set(#opctx.get_promise(&mut #scope, #promise_id).unwrap().into());
    }),
  );

  if config.async_lazy || config.async_deferred {
    let lazy = config.async_lazy;
    let deferred = config.async_deferred;
    output.extend(gs_quote!(generator_state(promise_id, result, opctx, scope) => {
      // Lazy and deferred results will always return None
      deno_core::_ops::#mapper(#opctx, #lazy, #deferred, #promise_id, #result, |#scope, #result| {
        #return_value
      });
    }));
  } else {
    output.extend(gs_quote!(generator_state(promise_id, result, opctx, scope) => {
      if let Some(#result) = deno_core::_ops::#mapper(#opctx, false, false, #promise_id, #result, |#scope, #result| {
        #return_value
      }) {
        // Eager poll returned a value
        #return_value_immediate;
        return 0;
      }
    }));
  }
  output.extend(quote!(return 2;));

  let with_opstate =
    if generator_state.needs_opstate | generator_state.needs_stack_trace {
      with_opstate(generator_state)
    } else {
      quote!()
    };

  let with_opctx =
    if generator_state.needs_opctx | generator_state.needs_stack_trace {
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

  let with_scope =
    if generator_state.needs_scope | generator_state.needs_stack_trace {
      with_scope(generator_state)
    } else {
      quote!()
    };

  let with_stack_trace = if generator_state.needs_stack_trace {
    with_stack_trace(generator_state)
  } else {
    quote!()
  };

  Ok(
    gs_quote!(generator_state(info, slow_function, slow_function_metrics, opctx) => {
      fn slow_function_impl<'s>(info: &'s deno_core::v8::FunctionCallbackInfo) -> usize {
        #[cfg(debug_assertions)]
        let _reentrancy_check_guard = deno_core::_ops::reentrancy_check(&<Self as deno_core::_ops::Op>::DECL);

        #with_scope
        #with_retval
        #with_args
        #with_opctx
        #with_opstate
        #with_self
        #with_stack_trace

        #output
      }

      extern "C" fn #slow_function<'s>(#info: *const deno_core::v8::FunctionCallbackInfo) {
        let info: &'s _ = unsafe { &*#info };
        Self::slow_function_impl(info);
      }

      extern "C" fn #slow_function_metrics<'s>(#info: *const deno_core::v8::FunctionCallbackInfo) {
        let info: &'s _ = unsafe { &*#info };
        let args = deno_core::v8::FunctionCallbackArguments::from_function_callback_info(info);
        let #opctx: &'s _ = unsafe {
          &*(deno_core::v8::Local::<deno_core::v8::External>::cast_unchecked(args.data()).value()
            as *const deno_core::_ops::OpCtx)
        };
        deno_core::_ops::dispatch_metrics_async(#opctx, deno_core::_ops::OpMetricsEvent::Dispatched);
        let res = Self::slow_function_impl(info);
        if res == 0 {
          deno_core::_ops::dispatch_metrics_async(#opctx, deno_core::_ops::OpMetricsEvent::Completed);
        } else if res == 1 {
          deno_core::_ops::dispatch_metrics_async(#opctx, deno_core::_ops::OpMetricsEvent::Error);
        }
      }
    }),
  )
}
