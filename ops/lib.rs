// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
#![doc = include_str!("README.md")]

use attrs::Attributes;
use optimizer::BailoutReason;
use optimizer::Optimizer;
use proc_macro::TokenStream;
use proc_macro2::Span;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use quote::ToTokens;
use std::error::Error;
use syn::parse;
use syn::parse_macro_input;
use syn::punctuated::Punctuated;
use syn::token::Comma;
use syn::FnArg;
use syn::GenericParam;
use syn::Ident;
use syn::ItemFn;
use syn::Lifetime;
use syn::LifetimeDef;

mod attrs;
mod deno;
mod fast_call;
mod op2;
mod optimizer;

const SCOPE_LIFETIME: &str = "'scope";

/// Add the 'scope lifetime to the function signature.
fn add_scope_lifetime(func: &mut ItemFn) {
  let span = Span::call_site();
  let lifetime = LifetimeDef::new(Lifetime::new(SCOPE_LIFETIME, span));
  let generics = &mut func.sig.generics;
  if !generics.lifetimes().any(|def| *def == lifetime) {
    generics.params.push(GenericParam::Lifetime(lifetime));
  }
}

struct Op {
  orig: ItemFn,
  item: ItemFn,
  /// Is this an async op?
  ///   - `async fn`
  ///   - returns a Future
  is_async: bool,
  // optimizer: Optimizer,
  core: TokenStream2,
  attrs: Attributes,
}

impl Op {
  fn new(item: ItemFn, attrs: Attributes) -> Self {
    // Preserve the original function. Change the name to `call`.
    //
    // impl op_foo {
    //   fn call() {}
    //   ...
    // }
    let mut orig = item.clone();
    orig.sig.ident = Ident::new("call", Span::call_site());

    let is_async = item.sig.asyncness.is_some() || is_future(&item.sig.output);
    let scope_params = exclude_non_lifetime_params(&item.sig.generics.params);
    orig.sig.generics.params = scope_params;
    orig.sig.generics.where_clause.take();
    add_scope_lifetime(&mut orig);

    #[cfg(test)]
    let core = quote!(deno_core);
    #[cfg(not(test))]
    let core = deno::import();

    Self {
      orig,
      item,
      is_async,
      core,
      attrs,
    }
  }

  fn gen(mut self) -> TokenStream2 {
    let mut optimizer = Optimizer::new();
    match optimizer.analyze(&mut self) {
      Err(BailoutReason::MustBeSingleSegment)
      | Err(BailoutReason::FastUnsupportedParamType) => {
        optimizer.fast_compatible = false;
      }
      _ => {}
    };

    let Self {
      core,
      item,
      is_async,
      orig,
      attrs,
    } = self;
    let name = &item.sig.ident;

    // TODO(mmastrac): this code is a little awkward but eventually it'll disappear in favour of op2
    let mut generics = item.sig.generics.clone();
    generics.where_clause.take();
    generics.params = exclude_lifetime_params(&generics.params);
    let params = &generics.params.iter().collect::<Vec<_>>();
    let where_clause = &item.sig.generics.where_clause;

    // First generate fast call bindings to opt-in to error handling in slow call
    let fast_call::FastImplItems {
      impl_and_fn,
      decl,
      active,
    } = fast_call::generate(&core, &mut optimizer, &item);

    let docline = format!("Use `{name}::decl()` to get an op-declaration");

    let is_v8 = attrs.is_v8;
    let is_unstable = attrs.is_unstable;

    if let Some(v8_fn) = attrs.relation {
      return quote! {
        #[allow(non_camel_case_types)]
        #[doc="Auto-generated by `deno_ops`, i.e: `#[op]`"]
        #[doc=""]
        #[doc=#docline]
        #[doc="you can include in a `deno_core::Extension`."]
        pub struct #name #generics {
          _phantom_data: ::std::marker::PhantomData<(#(#params),*)>
        }

        impl #generics #core::_ops::Op for #name #generics #where_clause {
          const NAME: &'static str = stringify!(#name);
          const DECL: #core::OpDecl = #core::_ops::OpDecl::new_internal(
            Self::name(),
            #is_async,
            #is_unstable,
            #is_v8,
            // TODO(mmastrac)
            /*arg_count*/ 0,
            /*slow*/ #v8_fn::v8_fn_ptr as _,
            /*fast*/ #decl,
          );
        }

        #[doc(hidden)]
        impl #generics #name #generics #where_clause {
          pub const fn name() -> &'static str {
            stringify!(#name)
          }

          #[deprecated(note = "Use the const op::DECL instead")]
          pub const fn decl() -> #core::_ops::OpDecl {
            <Self as #core::_ops::Op>::DECL
          }

          #[inline]
          #[allow(clippy::too_many_arguments)]
          #[allow(clippy::extra_unused_lifetimes)]
          #orig
        }

        #impl_and_fn
      };
    }

    let has_fallible_fast_call = active && optimizer.returns_result;

    let (v8_body, arg_count) = if is_async {
      let deferred: bool = attrs.deferred;
      codegen_v8_async(
        &core,
        &item,
        attrs,
        item.sig.asyncness.is_some(),
        deferred,
      )
    } else {
      codegen_v8_sync(&core, &item, attrs, has_fallible_fast_call)
    };

    // Generate wrapper
    quote! {
      #[allow(non_camel_case_types)]
      #[doc="Auto-generated by `deno_ops`, i.e: `#[op]`"]
      #[doc=""]
      #[doc=#docline]
      #[doc="you can include in a `deno_core::Extension`."]
      pub struct #name #generics {
        _phantom_data: ::std::marker::PhantomData<(#(#params),*)>
      }

      impl #generics #core::_ops::Op for #name #generics #where_clause {
        const NAME: &'static str = stringify!(#name);
        const DECL: #core::OpDecl = #core::_ops::OpDecl::new_internal(
          Self::name(),
          #is_async,
          #is_unstable,
          #is_v8,
          #arg_count as u8,
          /*slow*/ Self::v8_fn_ptr as _,
          /*fast*/ #decl,
        );
      }

      #[doc(hidden)]
      impl #generics #name #generics #where_clause {
        pub const fn name() -> &'static str {
          stringify!(#name)
        }

        #[allow(clippy::not_unsafe_ptr_arg_deref)]
        pub extern "C" fn v8_fn_ptr (info: *const #core::v8::FunctionCallbackInfo) {
          let info = unsafe { &*info };
          let scope = &mut unsafe { #core::v8::CallbackScope::new(info) };
          let args = #core::v8::FunctionCallbackArguments::from_function_callback_info(info);
          let rv = #core::v8::ReturnValue::from_function_callback_info(info);
          Self::v8_func(scope, args, rv);
        }

        #[deprecated(note = "Use the const op::DECL instead")]
        pub const fn decl() -> #core::_ops::OpDecl {
          <Self as #core::_ops::Op>::DECL
        }

        #[inline]
        #[allow(clippy::too_many_arguments)]
        #[allow(clippy::extra_unused_lifetimes)]
        #orig

        pub fn v8_func<'scope>(
          scope: &mut #core::v8::HandleScope<'scope>,
          args: #core::v8::FunctionCallbackArguments,
          mut rv: #core::v8::ReturnValue,
        ) {
          #v8_body
        }
      }

      #impl_and_fn
    }
  }
}

/// Deprecated. Use [`macro@op2`].
#[doc = include_str!("op1.md")]
#[proc_macro_attribute]
pub fn op(attr: TokenStream, item: TokenStream) -> TokenStream {
  let margs = parse_macro_input!(attr as Attributes);
  let func = parse::<ItemFn>(item).expect("expected a function");
  let op = Op::new(func, margs);
  op.gen().into()
}

/// A macro designed to provide an extremely fast V8->Rust interface layer.
#[doc = include_str!("op2/README.md")]
#[proc_macro_attribute]
pub fn op2(attr: TokenStream, item: TokenStream) -> TokenStream {
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

/// Generate the body of a v8 func for an async op
fn codegen_v8_async(
  core: &TokenStream2,
  f: &syn::ItemFn,
  margs: Attributes,
  asyncness: bool,
  deferred: bool,
) -> (TokenStream2, usize) {
  let Attributes { is_v8, .. } = margs;
  let special_args = f
    .sig
    .inputs
    .iter()
    .map_while(|a| {
      (if is_v8 { scope_arg(a) } else { None })
        .or_else(|| rc_refcell_opstate_arg(a))
    })
    .collect::<Vec<_>>();
  let rust_i0 = special_args.len();
  let args_head = special_args.into_iter().collect::<TokenStream2>();

  let (arg_decls, args_tail, _) = codegen_args(core, f, rust_i0, 1, asyncness);

  let wrapper = match (asyncness, is_result(&f.sig.output)) {
    (true, true) => {
      quote! {
        let fut = #core::_ops::map_async_op1(ctx, Self::call(#args_head #args_tail));
        let maybe_response = #core::_ops::queue_async_op(
          ctx,
          scope,
          #deferred,
          promise_id,
          fut,
        );
      }
    }
    (true, false) => {
      quote! {
        let fut = #core::_ops::map_async_op2(ctx, Self::call(#args_head #args_tail));
        let maybe_response = #core::_ops::queue_async_op(
          ctx,
          scope,
          #deferred,
          promise_id,
          fut,
        );
      }
    }
    (false, true) => {
      quote! {
        let fut = #core::_ops::map_async_op3(ctx, Self::call(#args_head #args_tail));
        let maybe_response = #core::_ops::queue_async_op(
          ctx,
          scope,
          #deferred,
          promise_id,
          fut,
        );
      }
    }
    (false, false) => {
      quote! {
        let fut = #core::_ops::map_async_op4(ctx, Self::call(#args_head #args_tail));
        let maybe_response = #core::_ops::queue_async_op(
          ctx,
          scope,
          #deferred,
          promise_id,
          fut,
        );
      }
    }
  };

  let token_stream = quote! {
    use #core::futures::FutureExt;
    // SAFETY: #core guarantees args.data() is a v8 External pointing to an OpCtx for the isolates lifetime
    let ctx = unsafe {
      &*(#core::v8::Local::<#core::v8::External>::cast(args.data()).value()
      as *const #core::_ops::OpCtx)
    };

    let promise_id = args.get(0);
    let promise_id = #core::v8::Local::<#core::v8::Integer>::try_from(promise_id)
      .map(|l| l.value() as #core::PromiseId)
      .map_err(#core::anyhow::Error::from);
    // Fail if promise id invalid (not an int)
    let promise_id: #core::PromiseId = match promise_id {
      Ok(promise_id) => promise_id,
      Err(err) => {
        #core::_ops::throw_type_error(scope, format!("invalid promise id: {}", err));
        return;
      }
    };

    #arg_decls
    #wrapper

    if let Some(response) = maybe_response {
      rv.set(response);
    }
  };

  // +1 arg for the promise ID
  (token_stream, 1 + f.sig.inputs.len() - rust_i0)
}

fn scope_arg(arg: &FnArg) -> Option<TokenStream2> {
  if is_handle_scope(arg) {
    Some(quote! { scope, })
  } else {
    None
  }
}

fn opstate_arg(arg: &FnArg) -> Option<TokenStream2> {
  match arg {
    arg if is_rc_refcell_opstate(arg) => Some(quote! { ctx.state.clone(), }),
    arg if is_mut_ref_opstate(arg) => {
      Some(quote! { &mut std::cell::RefCell::borrow_mut(&ctx.state), })
    }
    _ => None,
  }
}

fn rc_refcell_opstate_arg(arg: &FnArg) -> Option<TokenStream2> {
  match arg {
    arg if is_rc_refcell_opstate(arg) => Some(quote! { ctx.state.clone(), }),
    arg if is_mut_ref_opstate(arg) => Some(
      quote! { compile_error!("mutable opstate is not supported in async ops"), },
    ),
    _ => None,
  }
}

/// Generate the body of a v8 func for a sync op
fn codegen_v8_sync(
  core: &TokenStream2,
  f: &syn::ItemFn,
  margs: Attributes,
  has_fallible_fast_call: bool,
) -> (TokenStream2, usize) {
  let Attributes { is_v8, .. } = margs;
  let special_args = f
    .sig
    .inputs
    .iter()
    .map_while(|a| {
      (if is_v8 { scope_arg(a) } else { None }).or_else(|| opstate_arg(a))
    })
    .collect::<Vec<_>>();
  let rust_i0 = special_args.len();
  let args_head = special_args.into_iter().collect::<TokenStream2>();
  let (arg_decls, args_tail, _) = codegen_args(core, f, rust_i0, 0, false);
  let ret = codegen_sync_ret(core, &f.sig.output);

  let fast_error_handler = if has_fallible_fast_call {
    quote! {
      {
        let op_state = &mut std::cell::RefCell::borrow_mut(&ctx.state);
        if let Some(err) = op_state.last_fast_op_error.take() {
          let exception = #core::error::to_v8_error(scope, op_state.get_error_class_fn, &err);
          scope.throw_exception(exception);
          return;
        }
      }
    }
  } else {
    quote! {}
  };

  let token_stream = quote! {
    // SAFETY: #core guarantees args.data() is a v8 External pointing to an OpCtx for the isolates lifetime
    let ctx = unsafe {
      &*(#core::v8::Local::<#core::v8::External>::cast(args.data()).value()
      as *const #core::_ops::OpCtx)
    };

    #fast_error_handler
    #arg_decls

    let result = Self::call(#args_head #args_tail);

    // use RefCell::borrow instead of state.borrow to avoid clash with std::borrow::Borrow
    let op_state = ::std::cell::RefCell::borrow(&*ctx.state);
    op_state.tracker.track_sync(ctx.id);

    #ret
  };

  (token_stream, f.sig.inputs.len() - rust_i0)
}

/// (full declarations, idents, v8 argument count)
type ArgumentDecl = (TokenStream2, TokenStream2, usize);

fn codegen_args(
  core: &TokenStream2,
  f: &syn::ItemFn,
  rust_i0: usize, // Index of first generic arg in rust
  v8_i0: usize,   // Index of first generic arg in v8/js
  asyncness: bool,
) -> ArgumentDecl {
  let inputs = &f.sig.inputs.iter().skip(rust_i0).enumerate();
  let ident_seq: TokenStream2 = inputs
    .clone()
    .map(|(i, _)| format!("arg_{i}"))
    .collect::<Vec<_>>()
    .join(", ")
    .parse()
    .unwrap();
  let decls: TokenStream2 = inputs
    .clone()
    .map(|(i, arg)| {
      codegen_arg(core, arg, format!("arg_{i}").as_ref(), v8_i0 + i, asyncness)
    })
    .collect();
  (decls, ident_seq, inputs.len())
}

fn codegen_arg(
  core: &TokenStream2,
  arg: &syn::FnArg,
  name: &str,
  idx: usize,
  asyncness: bool,
) -> TokenStream2 {
  let ident = quote::format_ident!("{name}");
  let (pat, ty) = match arg {
    syn::FnArg::Typed(pat) => {
      if is_optional_fast_callback_option(&pat.ty)
        || is_optional_wasm_memory(&pat.ty)
      {
        return quote! { let #ident = None; };
      }
      (&pat.pat, &pat.ty)
    }
    _ => unreachable!(),
  };
  // Fast path if arg should be skipped
  if matches!(**pat, syn::Pat::Wild(_)) {
    return quote! { let #ident = (); };
  }
  // Fast path for `String`
  if let Some(is_ref) = is_string(&**ty) {
    let ref_block = if is_ref {
      quote! { let #ident = #ident.as_ref(); }
    } else {
      quote! {}
    };
    return quote! {
      let #ident = match #core::v8::Local::<#core::v8::String>::try_from(args.get(#idx as i32)) {
        Ok(v8_string) => #core::serde_v8::to_utf8(v8_string, scope),
        Err(_) => {
          return #core::_ops::throw_type_error(scope, format!("Expected string at position {}", #idx));
        }
      };
      #ref_block
    };
  }
  // Fast path for `Cow<'_, str>`
  if is_cow_str(&**ty) {
    return quote! {
      let #ident = match #core::v8::Local::<#core::v8::String>::try_from(args.get(#idx as i32)) {
        Ok(v8_string) => ::std::borrow::Cow::Owned(#core::serde_v8::to_utf8(v8_string, scope)),
        Err(_) => {
          return #core::_ops::throw_type_error(scope, format!("Expected string at position {}", #idx));
        }
      };
    };
  }
  // Fast path for `Option<String>`
  if is_option_string(&**ty) {
    return quote! {
      let #ident = match #core::v8::Local::<#core::v8::String>::try_from(args.get(#idx as i32)) {
        Ok(v8_string) => Some(#core::serde_v8::to_utf8(v8_string, scope)),
        Err(_) => None
      };
    };
  }
  // Fast path for &/&mut [u8] and &/&mut [u32]
  match is_ref_slice(&**ty) {
    None => {}
    Some(SliceType::U32Mut) => {
      assert!(!asyncness, "Memory slices are not allowed in async ops");
      let blck = codegen_u32_mut_slice(core, idx);
      return quote! {
        let #ident = #blck;
      };
    }
    Some(SliceType::F64Mut) => {
      assert!(!asyncness, "Memory slices are not allowed in async ops");
      let blck = codegen_f64_mut_slice(core, idx);
      return quote! {
        let #ident = #blck;
      };
    }
    Some(_) => {
      assert!(!asyncness, "Memory slices are not allowed in async ops");
      let blck = codegen_u8_slice(core, idx);
      return quote! {
        let #ident = #blck;
      };
    }
  }
  // Fast path for `*const u8`
  if is_ptr_u8(&**ty) {
    let blk = codegen_u8_ptr(core, idx);
    return quote! {
      let #ident = #blk;
    };
  }
  // Fast path for `*const c_void` and `*mut c_void`
  if is_ptr_cvoid(&**ty) {
    let blk = codegen_cvoid_ptr(core, idx);
    return quote! {
      let #ident = #blk;
    };
  }
  // Otherwise deserialize it via serde_v8
  quote! {
    let #ident = args.get(#idx as i32);
    let #ident = match #core::serde_v8::from_v8(scope, #ident) {
      Ok(v) => v,
      Err(err) => {
        let msg = format!("Error parsing args at position {}: {}", #idx, #core::anyhow::Error::from(err));
        return #core::_ops::throw_type_error(scope, msg);
      }
    };
  }
}

fn codegen_u8_slice(core: &TokenStream2, idx: usize) -> TokenStream2 {
  quote! {{
    let value = args.get(#idx as i32);
    match #core::v8::Local::<#core::v8::ArrayBuffer>::try_from(value) {
      Ok(b) => {
        let byte_length = b.byte_length();
        if let Some(data) = b.data() {
          let store = data.cast::<u8>().as_ptr();
          // SAFETY: rust guarantees that lifetime of slice is no longer than the call.
          unsafe { ::std::slice::from_raw_parts_mut(store, byte_length) }
        } else {
          &mut []
        }
      },
      Err(_) => {
        if let Ok(view) = #core::v8::Local::<#core::v8::ArrayBufferView>::try_from(value) {
          let len = view.byte_length();
          let offset = view.byte_offset();
          let buffer = match view.buffer(scope) {
              Some(v) => v,
              None => {
                return #core::_ops::throw_type_error(scope, format!("Expected ArrayBufferView at position {}", #idx));
              }
          };
          if let Some(data) = buffer.data() {
            let store = data.cast::<u8>().as_ptr();
            // SAFETY: rust guarantees that lifetime of slice is no longer than the call.
            unsafe { ::std::slice::from_raw_parts_mut(store.add(offset), len) }
          } else {
            &mut []
          }
        } else {
          return #core::_ops::throw_type_error(scope, format!("Expected ArrayBufferView at position {}", #idx));
        }
      }
    }}
  }
}

fn codegen_u8_ptr(core: &TokenStream2, idx: usize) -> TokenStream2 {
  quote! {{
    let value = args.get(#idx as i32);
    match #core::v8::Local::<#core::v8::ArrayBuffer>::try_from(value) {
      Ok(b) => {
        if let Some(data) = b.data() {
          data.cast::<u8>().as_ptr()
        } else {
          std::ptr::null::<u8>()
        }
      },
      Err(_) => {
        if let Ok(view) = #core::v8::Local::<#core::v8::ArrayBufferView>::try_from(value) {
          let offset = view.byte_offset();
          let buffer = match view.buffer(scope) {
              Some(v) => v,
              None => {
                return #core::_ops::throw_type_error(scope, format!("Expected ArrayBufferView at position {}", #idx));
              }
          };
          let store = if let Some(data) = buffer.data() {
            data.cast::<u8>().as_ptr()
          } else {
            std::ptr::null_mut::<u8>()
          };
          unsafe { store.add(offset) }
        } else {
          return #core::_ops::throw_type_error(scope, format!("Expected ArrayBufferView at position {}", #idx));
        }
      }
    }
  }}
}

fn codegen_cvoid_ptr(core: &TokenStream2, idx: usize) -> TokenStream2 {
  quote! {{
    let value = args.get(#idx as i32);
    if value.is_null() {
      std::ptr::null_mut()
    } else if let Ok(b) = #core::v8::Local::<#core::v8::External>::try_from(value) {
      b.value()
    } else {
      return #core::_ops::throw_type_error(scope, format!("Expected External at position {}", #idx));
    }
  }}
}

fn codegen_u32_mut_slice(core: &TokenStream2, idx: usize) -> TokenStream2 {
  quote! {
    if let Ok(view) = #core::v8::Local::<#core::v8::Uint32Array>::try_from(args.get(#idx as i32)) {
      let (offset, len) = (view.byte_offset(), view.byte_length());
      let buffer = match view.buffer(scope) {
          Some(v) => v,
          None => {
            return #core::_ops::throw_type_error(scope, format!("Expected Uint32Array at position {}", #idx));
          }
      };
      if let Some(data) = buffer.data() {
        let store = data.cast::<u8>().as_ptr();
        // SAFETY: buffer from Uint32Array. Rust guarantees that lifetime of slice is no longer than the call.
        unsafe { ::std::slice::from_raw_parts_mut(store.add(offset) as *mut u32, len / 4) }
      } else {
        &mut []
      }
    } else {
      return #core::_ops::throw_type_error(scope, format!("Expected Uint32Array at position {}", #idx));
    }
  }
}

fn codegen_f64_mut_slice(core: &TokenStream2, idx: usize) -> TokenStream2 {
  quote! {
    if let Ok(view) = #core::v8::Local::<#core::v8::Float64Array>::try_from(args.get(#idx as i32)) {
      let (offset, len) = (view.byte_offset(), view.byte_length());
      let buffer = match view.buffer(scope) {
          Some(v) => v,
          None => {
            return #core::_ops::throw_type_error(scope, format!("Expected Float64Array at position {}", #idx));
          }
      };
      if let Some(data) = buffer.data() {
        let store = data.cast::<u8>().as_ptr();
        unsafe { ::std::slice::from_raw_parts_mut(store.add(offset) as *mut f64, len / 8) }
      } else {
        &mut []
      }
    } else {
      return #core::_ops::throw_type_error(scope, format!("Expected Float64Array at position {}", #idx));
    }
  }
}

fn codegen_sync_ret(
  core: &TokenStream2,
  output: &syn::ReturnType,
) -> TokenStream2 {
  if is_void(output) {
    return quote! {};
  }

  if is_u32_rv(output) {
    return quote! {
      rv.set_uint32(result as u32);
    };
  }

  // Optimize Result<(), Err> to skip serde_v8 when Ok(...)
  let ok_block = if is_unit_result(output) {
    quote! {}
  } else if is_u32_rv_result(output) {
    quote! {
      rv.set_uint32(result as u32);
    }
  } else if is_ptr_cvoid(output) || is_ptr_cvoid_rv(output) {
    quote! {
      if result.is_null() {
        // External cannot contain a null pointer, null pointers are instead represented as null.
        rv.set_null();
      } else {
        rv.set(v8::External::new(scope, result as *mut ::std::ffi::c_void).into());
      }
    }
  } else {
    quote! {
      match #core::serde_v8::to_v8(scope, result) {
        Ok(ret) => rv.set(ret),
        Err(err) => #core::_ops::throw_type_error(
          scope,
          format!("Error serializing return: {}", #core::anyhow::Error::from(err)),
        ),
      };
    }
  };

  if !is_result(output) {
    return ok_block;
  }

  quote! {
    match result {
      Ok(result) => {
        #ok_block
      },
      Err(err) => {
        let exception = #core::error::to_v8_error(scope, op_state.get_error_class_fn, &err);
        scope.throw_exception(exception);
      },
    };
  }
}

fn is_void(ty: impl ToTokens) -> bool {
  tokens(ty).is_empty()
}

fn is_result(ty: impl ToTokens) -> bool {
  let tokens = tokens(ty);
  if tokens.trim_start_matches("-> ").starts_with("Result <") {
    return true;
  }
  // Detect `io::Result<...>`, `anyhow::Result<...>`, etc...
  // i.e: Result aliases/shorthands which are unfortunately "opaque" at macro-time
  match tokens.find(":: Result <") {
    Some(idx) => !tokens.split_at(idx).0.contains('<'),
    None => false,
  }
}

fn is_string(ty: impl ToTokens) -> Option<bool> {
  let toks = tokens(ty);
  if toks == "String" {
    return Some(false);
  }
  if toks == "& str" {
    return Some(true);
  }
  None
}

fn is_option_string(ty: impl ToTokens) -> bool {
  tokens(ty) == "Option < String >"
}

fn is_cow_str(ty: impl ToTokens) -> bool {
  tokens(&ty).starts_with("Cow <") && tokens(&ty).ends_with("str >")
}

enum SliceType {
  U8,
  U8Mut,
  U32Mut,
  F64Mut,
}

fn is_ref_slice(ty: impl ToTokens) -> Option<SliceType> {
  if is_u8_slice(&ty) {
    return Some(SliceType::U8);
  }
  if is_u8_slice_mut(&ty) {
    return Some(SliceType::U8Mut);
  }
  if is_u32_slice_mut(&ty) {
    return Some(SliceType::U32Mut);
  }
  if is_f64_slice_mut(&ty) {
    return Some(SliceType::F64Mut);
  }
  None
}

fn is_u8_slice(ty: impl ToTokens) -> bool {
  tokens(ty) == "& [u8]"
}

fn is_u8_slice_mut(ty: impl ToTokens) -> bool {
  tokens(ty) == "& mut [u8]"
}

fn is_u32_slice_mut(ty: impl ToTokens) -> bool {
  tokens(ty) == "& mut [u32]"
}

fn is_f64_slice_mut(ty: impl ToTokens) -> bool {
  tokens(ty) == "& mut [f64]"
}

fn is_ptr_u8(ty: impl ToTokens) -> bool {
  tokens(ty) == "* const u8"
}

fn is_ptr_cvoid(ty: impl ToTokens) -> bool {
  tokens(&ty) == "* const c_void" || tokens(&ty) == "* mut c_void"
}

fn is_ptr_cvoid_rv(ty: impl ToTokens) -> bool {
  tokens(&ty).contains("Result < * const c_void")
    || tokens(&ty).contains("Result < * mut c_void")
}

fn is_optional_fast_callback_option(ty: impl ToTokens) -> bool {
  tokens(&ty).contains("Option < & mut FastApiCallbackOptions")
}

fn is_optional_wasm_memory(ty: impl ToTokens) -> bool {
  tokens(&ty).contains("Option < & mut [u8]")
}

/// Detects if the type can be set using `rv.set_uint32` fast path
fn is_u32_rv(ty: impl ToTokens) -> bool {
  ["u32", "u8", "u16"].iter().any(|&s| tokens(&ty) == s) || is_resource_id(&ty)
}

/// Detects if the type is of the format Result<u32/u8/u16, Err>
fn is_u32_rv_result(ty: impl ToTokens) -> bool {
  is_result(&ty)
    && (tokens(&ty).contains("Result < u32")
      || tokens(&ty).contains("Result < u8")
      || tokens(&ty).contains("Result < u16")
      || is_resource_id(&ty))
}

/// Detects if a type is of the form Result<(), Err>
fn is_unit_result(ty: impl ToTokens) -> bool {
  is_result(&ty) && tokens(&ty).contains("Result < ()")
}

fn is_resource_id(arg: impl ToTokens) -> bool {
  let re = lazy_regex::regex!(r#": (?:deno_core :: )?ResourceId$"#);
  re.is_match(&tokens(arg))
}

fn is_mut_ref_opstate(arg: impl ToTokens) -> bool {
  let re = lazy_regex::regex!(r#": & mut (?:deno_core :: )?OpState$"#);
  re.is_match(&tokens(arg))
}

fn is_rc_refcell_opstate(arg: &syn::FnArg) -> bool {
  let re =
    lazy_regex::regex!(r#": Rc < RefCell < (?:deno_core :: )?OpState > >$"#);
  re.is_match(&tokens(arg))
}

fn is_handle_scope(arg: &syn::FnArg) -> bool {
  let re = lazy_regex::regex!(
    r#": & mut (?:deno_core :: )?v8 :: HandleScope(?: < '\w+ >)?$"#
  );
  re.is_match(&tokens(arg))
}

fn is_future(ty: impl ToTokens) -> bool {
  tokens(&ty).contains("impl Future < Output =")
}

fn tokens(x: impl ToTokens) -> String {
  x.to_token_stream().to_string()
}

fn exclude_lifetime_params(
  generic_params: &Punctuated<GenericParam, Comma>,
) -> Punctuated<GenericParam, Comma> {
  generic_params
    .iter()
    .filter(|t| !tokens(t).starts_with('\''))
    .cloned()
    .collect::<Punctuated<GenericParam, Comma>>()
}

fn exclude_non_lifetime_params(
  generic_params: &Punctuated<GenericParam, Comma>,
) -> Punctuated<GenericParam, Comma> {
  generic_params
    .iter()
    .filter(|t| tokens(t).starts_with('\''))
    .cloned()
    .collect::<Punctuated<GenericParam, Comma>>()
}

#[cfg(test)]
mod tests {
  use crate::Attributes;
  use crate::Op;
  use pretty_assertions::assert_eq;
  use std::path::PathBuf;

  #[testing_macros::fixture("optimizer_tests/**/*.rs")]
  fn test_codegen(input: PathBuf) {
    let update_expected = std::env::var("UPDATE_EXPECTED").is_ok();

    let source =
      std::fs::read_to_string(&input).expect("Failed to read test file");

    let mut attrs = Attributes::default();
    if source.contains("// @test-attr:fast") {
      attrs.must_be_fast = true;
    }
    if source.contains("// @test-attr:wasm") {
      attrs.is_wasm = true;
      attrs.must_be_fast = true;
    }

    let item = syn::parse_str(&source).expect("Failed to parse test file");
    let op = Op::new(item, attrs);

    let expected = std::fs::read_to_string(input.with_extension("out"))
      .expect("Failed to read expected output file");

    // Print the raw tokens in case we fail to parse
    let actual = op.gen();
    println!("-----Raw tokens-----\n{}----------\n", actual);

    // Validate syntax tree.
    let tree = syn2::parse2(actual).unwrap();
    let actual = prettyplease::unparse(&tree);
    if update_expected {
      std::fs::write(input.with_extension("out"), actual)
        .expect("Failed to write expected file");
    } else {
      assert_eq!(actual, expected);
    }
  }
}
