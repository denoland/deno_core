// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use crate::op2::signature::*;
use deno_proc_macro_rules::rules;

use quote::ToTokens;

use syn2::ReturnType;

use syn2::Type;
use syn2::TypeParamBound;

/// One level of type unwrapping for a return value. We cannot rely on `proc-macro-rules` to correctly
/// unwrap `impl Future<...>`, so we do it by hand.
enum UnwrappedReturn {
  Type(Type),
  Result(Type),
  Future(Type),
}

fn unwrap_return(ty: &Type) -> Result<UnwrappedReturn, RetError> {
  use syn2 as syn;
  match ty {
    Type::ImplTrait(imp) => {
      if imp.bounds.len() != 1 {
        return Err(RetError::InvalidType(ArgError::InvalidType(
          stringify_token(ty),
        )));
      }
      if let Some(TypeParamBound::Trait(t)) = imp.bounds.first() {
        rules!(t.into_token_stream() => {
          ($($_package:ident ::)* Future < Output = $ty:ty >) => Ok(UnwrappedReturn::Future(ty)),
          ($ty:ty) => Err(RetError::InvalidType(ArgError::InvalidType(stringify_token(ty)))),
        })
      } else {
        Err(RetError::InvalidType(ArgError::InvalidType(
          stringify_token(ty),
        )))
      }
    }
    Type::Path(ty) => {
      rules!(ty.to_token_stream() => {
        // x::y::Result<Value>, like io::Result and other specialty result types
        ($($_package:ident ::)* Result < $ty:ty >) => {
          Ok(UnwrappedReturn::Result(ty))
        }
        // x::y::Result<Value, Error>
        ($($_package:ident ::)* Result < $ty:ty, $_error:ty >) => {
          Ok(UnwrappedReturn::Result(ty))
        }
        // Everything else
        ($ty:ty) => {
          Ok(UnwrappedReturn::Type(ty))
        }
      })
    }
    Type::Tuple(_) => Ok(UnwrappedReturn::Type(ty.clone())),
    _ => Err(RetError::InvalidType(ArgError::InvalidType(
      stringify_token(ty),
    ))),
  }
}

pub(crate) fn parse_return(
  is_async: bool,
  attrs: Attributes,
  rt: &ReturnType,
) -> Result<RetVal, RetError> {
  use UnwrappedReturn::*;

  let res = match rt {
    ReturnType::Default => RetVal::Infallible(Arg::Void),
    ReturnType::Type(_, rt) => match unwrap_return(rt)? {
      Type(ty) => RetVal::Infallible(parse_type(attrs, &ty)?),
      Result(ty) => match unwrap_return(&ty)? {
        Type(ty) => RetVal::Result(parse_type(attrs, &ty)?),
        Future(ty) => match unwrap_return(&ty)? {
          Type(ty) => RetVal::ResultFuture(parse_type(attrs, &ty)?),
          Result(ty) => RetVal::ResultFutureResult(parse_type(attrs, &ty)?),
          _ => {
            return Err(RetError::InvalidType(ArgError::InvalidType(
              stringify_token(rt),
            )))
          }
        },
        _ => {
          return Err(RetError::InvalidType(ArgError::InvalidType(
            stringify_token(rt),
          )))
        }
      },
      Future(ty) => match unwrap_return(&ty)? {
        Type(ty) => RetVal::Future(parse_type(attrs, &ty)?),
        Result(ty) => RetVal::FutureResult(parse_type(attrs, &ty)?),
        _ => {
          return Err(RetError::InvalidType(ArgError::InvalidType(
            stringify_token(rt),
          )))
        }
      },
    },
  };

  // If the signature was async, wrap this return value in one level of future.
  if is_async {
    let res = match res {
      RetVal::Infallible(t) => RetVal::Future(t),
      RetVal::Result(t) => RetVal::FutureResult(t),
      _ => {
        return Err(RetError::InvalidType(ArgError::InvalidType(
          stringify_token(rt),
        )))
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
  use syn2::parse_str;

  #[test]
  fn test_parse_result() {
    use Arg::*;
    use RetVal::*;

    for (expected, input) in [
      (Infallible(Void), "()"),
      (Result(Void), "Result<()>"),
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
      let actual = parse_return(false, Attributes::default(), &rt)
        .expect("Failed to parse return");
      assert_eq!(expected, actual);
    }
  }
}
