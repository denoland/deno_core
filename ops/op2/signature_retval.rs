// Copyright 2018-2025 the Deno authors. MIT license.

use crate::op2::signature::*;
use proc_macro_rules::rules;

use quote::ToTokens;

use syn::ReturnType;

use syn::Type;
use syn::TypeParamBound;

/// One level of type unwrapping for a return value. We cannot rely on `proc-macro-rules` to correctly
/// unwrap `impl Future<...>`, so we do it by hand.
enum UnwrappedReturn {
  Type(Type),
  Result(Type),
  Future(Type),
}

fn unwrap_return(ty: &Type) -> Result<UnwrappedReturn, RetError> {
  match ty {
    Type::ImplTrait(imp) => {
      if imp
        .bounds
        .iter()
        .filter(|b| {
          matches!(
            b,
            TypeParamBound::Lifetime(_)
              | TypeParamBound::Trait(_)
              | TypeParamBound::Verbatim(_)
          )
        })
        .count()
        > 1
      {
        return Err(RetError::InvalidType(ArgError::InvalidType(
          stringify_token(ty),
          "for impl trait bounds",
        )));
      }
      match imp.bounds.first() {
        Some(TypeParamBound::Trait(t)) => {
          rules!(t.into_token_stream() => {
            ($($_package:ident ::)* Future < Output = $ty:ty $(,)? >) => Ok(UnwrappedReturn::Future(ty)),
            ($ty:ty) => Err(RetError::InvalidType(ArgError::InvalidType(stringify_token(ty), "for impl Future"))),
          })
        }
        _ => Err(RetError::InvalidType(ArgError::InvalidType(
          stringify_token(ty),
          "for impl",
        ))),
      }
    }
    Type::Path(ty) => {
      rules!(ty.to_token_stream() => {
        // x::y::Result<Value>, like io::Result and other specialty result types
        ($($_package:ident ::)* Result < $ty:ty $(,)? >) => {
          Ok(UnwrappedReturn::Result(ty))
        }
        // x::y::Result<Value, Error>
        ($($_package:ident ::)* Result < $ty:ty, $_error:ty $(,)? >) => {
          Ok(UnwrappedReturn::Result(ty))
        }
        // Everything else
        ($ty:ty) => {
          Ok(UnwrappedReturn::Type(ty))
        }
      })
    }
    Type::Tuple(_) => Ok(UnwrappedReturn::Type(ty.clone())),
    Type::Ptr(_) => Ok(UnwrappedReturn::Type(ty.clone())),
    Type::Reference(_) => Ok(UnwrappedReturn::Type(ty.clone())),
    _ => Err(RetError::InvalidType(ArgError::InvalidType(
      stringify_token(ty),
      "for return type",
    ))),
  }
}

pub(crate) fn parse_return(
  is_async: bool,
  is_fake_async: bool,
  attrs: Attributes,
  rt: &ReturnType,
) -> Result<RetVal, RetError> {
  use UnwrappedReturn::*;

  let res = match rt {
    ReturnType::Default => RetVal::Infallible(Arg::Void, is_fake_async),
    ReturnType::Type(_, rt) => match unwrap_return(rt)? {
      Type(ty) => RetVal::Infallible(
        parse_type(Position::RetVal, attrs, &ty)?,
        is_fake_async,
      ),
      Result(ty) => match unwrap_return(&ty)? {
        Type(ty) => RetVal::Result(
          parse_type(Position::RetVal, attrs, &ty)?,
          is_fake_async,
        ),
        Future(ty) => match unwrap_return(&ty)? {
          Type(ty) => {
            RetVal::ResultFuture(parse_type(Position::RetVal, attrs, &ty)?)
          }
          Result(ty) => RetVal::ResultFutureResult(parse_type(
            Position::RetVal,
            attrs,
            &ty,
          )?),
          _ => {
            return Err(RetError::InvalidType(ArgError::InvalidType(
              stringify_token(rt),
              "for result of future",
            )));
          }
        },
        _ => {
          return Err(RetError::InvalidType(ArgError::InvalidType(
            stringify_token(rt),
            "for result",
          )));
        }
      },
      Future(ty) => match unwrap_return(&ty)? {
        Type(ty) => RetVal::Future(parse_type(Position::RetVal, attrs, &ty)?),
        Result(ty) => {
          RetVal::FutureResult(parse_type(Position::RetVal, attrs, &ty)?)
        }
        _ => {
          return Err(RetError::InvalidType(ArgError::InvalidType(
            stringify_token(rt),
            "for future",
          )));
        }
      },
    },
  };

  // If the signature was async, wrap this return value in one level of future.
  if is_async {
    let res = match res {
      RetVal::Infallible(t, ..) => RetVal::Future(t),
      RetVal::Result(t, ..) => RetVal::FutureResult(t),
      _ => {
        return Err(RetError::InvalidType(ArgError::InvalidType(
          stringify_token(rt),
          "for async return",
        )));
      }
    };
    Ok(res)
  } else {
    Ok(res)
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use syn::parse_str;

  #[test]
  fn test_parse_result() {
    use Arg::*;
    use RetVal::*;

    for (expected, input) in [
      (Infallible(Void, false), "()"),
      (Result(Void, false), "Result<()>"),
      (Result(Void, false), "Result<(), ()>"),
      (Result(Void, false), "Result<(), (),>"),
      (Future(Void), "impl Future<Output = ()>"),
      (FutureResult(Void), "impl Future<Output = Result<()>>"),
      (ResultFuture(Void), "Result<impl Future<Output = ()>>"),
      (
        ResultFutureResult(Void),
        "Result<impl Future<Output = Result<()>>>",
      ),
    ] {
      let rt = parse_str::<ReturnType>(&format!("-> {input}"))
        .expect("Failed to parse");
      let actual = parse_return(false, false, Attributes::default(), &rt)
        .expect("Failed to parse return");
      assert_eq!(expected, actual);
    }
  }
}
