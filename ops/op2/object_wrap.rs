// Copyright 2018-2025 the Deno authors. MIT license.

use proc_macro_rules::rules;
use proc_macro2::TokenStream;
use quote::ToTokens;
use quote::format_ident;
use quote::quote;
use syn::ImplItem;
use syn::ItemFn;
use syn::ItemImpl;
use syn::spanned::Spanned;

use crate::op2::MacroConfig;
use crate::op2::Op2Error;
use crate::op2::generate_op2;

use super::signature::is_attribute_special;

// Object wrap for Cppgc-backed objects
//
// This module generates the glue code declarations
// for `impl` blocks to create JS objects in Rust
// using the op2 infra.
//
// ```rust
// #[op]
// impl MyObject {
//    #[constructor] // <-- first attribute defines binding type
//    #[cppgc]       // <-- attributes for op2
//    fn new() -> MyObject {
//      MyObject::new()
//    }
//
//    #[static_method]
//    #[cppgc]
//    fn static_method() -> MyObject {
//      // ...
//    }
//
//    #[method]
//    #[smi]
//    fn method(&self) -> i32 {
//      // ...
//    }
//
//    #[getter]
//    fn x(&self) -> i32 {}
//
//    #[setter]
//    fn x(&self, x: i32) {}
// }
//
// The generated OpMethodDecl that can be passed to
// `deno_core::extension!` macro to register the object
//
// ```rust
// deno_core::extension!(
//   ...,
//   objects = [MyObject],
// )
// ```
//
// ```js
// import { MyObject } from "ext:core/ops";
// ```
//
// Supported bindings:
// - constructor
// - methods
// - static methods
// - getters
// - setters
//
pub(crate) fn generate_impl_ops(
  attrs: TokenStream,
  item: ItemImpl,
) -> Result<TokenStream, Op2Error> {
  enum ClassTy {
    Base,
    Inherit(syn::Type),
  }

  let maybe_inherits_type = rules!(attrs => {
    () => None,
    (base) => Some(ClassTy::Base),
    (inherit = $inherits_type:ty) => Some(ClassTy::Inherit(inherits_type)),
  });

  let mut tokens = TokenStream::new();

  let self_ty = &item.self_ty;
  let self_ty_ident = self_ty.to_token_stream().to_string();

  // State
  let mut constructor = None;
  let mut methods = Vec::new();
  let mut static_methods = Vec::new();

  for item in item.items {
    if let ImplItem::Fn(mut method) = item {
      let span = method.span();
      let (item_fn_attrs, attrs) =
        method.attrs.into_iter().partition(is_attribute_special);

      /* Convert snake_case to camelCase */
      method.sig.ident = format_ident!(
        "{}",
        stringcase::camel_case(&method.sig.ident.to_string())
      );

      let mut func = ItemFn {
        attrs: item_fn_attrs,
        vis: method.vis,
        sig: method.sig,
        block: Box::new(method.block),
      };

      let mut config = MacroConfig::from_attributes(span, attrs)?;

      if maybe_inherits_type.is_some() {
        config.use_proto_cppgc = true;
      }

      if let Some(ref rename) = config.rename {
        func.sig.ident = format_ident!("{}", rename);
      }

      let ident = func.sig.ident.clone();
      if config.constructor {
        if constructor.is_some() {
          return Err(Op2Error::MultipleConstructors);
        }

        constructor = Some(ident);
      } else if config.static_member {
        static_methods.push(format_ident!("__static_{}", ident));
      } else {
        if config.setter {
          methods.push(format_ident!("__set_{}", ident));
        } else {
          methods.push(ident);
        }

        config.method = Some(self_ty_ident.clone());
      }

      config.self_name = Some(self_ty_ident.clone());

      let op = generate_op2(config, func)?;
      tokens.extend(op);
    }
  }

  let constructor = if let Some(constructor) = constructor {
    quote! { Some(#self_ty::#constructor()) }
  } else {
    quote! { None }
  };

  let (prototype_index, inherits_type_name) = match &maybe_inherits_type {
    Some(ClassTy::Base) => (
      quote! {
        impl deno_core::cppgc::PrototypeChain for #self_ty {
          fn prototype_index() -> Option<usize> {
            Some(0)
          }
        }
      },
      quote! {
        inherits_type_name: || None,
      },
    ),
    Some(ClassTy::Inherit(inherits_type)) => (
      quote! {
        impl deno_core::cppgc::PrototypeChain for #self_ty {
          fn prototype_index() -> Option<usize> {
            Some(<#inherits_type as deno_core::cppgc::PrototypeChain>::prototype_index().unwrap_or_default() + 1)
          }
        }
      },
      quote! {
        inherits_type_name: || Some(std::any::type_name::<#inherits_type>()),
      },
    ),
    None => (
      quote! {},
      quote! {
        inherits_type_name: || None,
      },
    ),
  };

  let res = quote! {
      #prototype_index

      impl #self_ty {
        pub const DECL: deno_core::_ops::OpMethodDecl = deno_core::_ops::OpMethodDecl {
          methods: &[
            #(
              #self_ty::#methods(),
            )*
          ],
          static_methods: &[
            #(
              #self_ty::#static_methods(),
            )*
          ],
          constructor: #constructor,
          name: ::deno_core::__op_name_fast!(#self_ty),
          type_name: || std::any::type_name::<#self_ty>(),
          #inherits_type_name
        };

        #tokens
      }
  };

  Ok(res)
}
