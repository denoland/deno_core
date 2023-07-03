// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use deno_proc_macro_rules::rules;
use proc_macro2::Ident;
use proc_macro2::Span;
use quote::quote;
use quote::ToTokens;
use std::collections::BTreeMap;
use strum::IntoEnumIterator;
use strum::IntoStaticStr;
use strum_macros::EnumIter;
use strum_macros::EnumString;
use syn2::Attribute;
use syn2::FnArg;
use syn2::GenericParam;
use syn2::Generics;
use syn2::Pat;
use syn2::ReturnType;
use syn2::Signature;
use syn2::Type;
use syn2::TypePath;
use thiserror::Error;

#[allow(non_camel_case_types)]
#[derive(
  Copy, Clone, Debug, Eq, PartialEq, IntoStaticStr, EnumString, EnumIter,
)]
pub enum NumericArg {
  /// A placeholder argument for arguments annotated with #[smi].
  __SMI__,
  /// A placeholder argument for void data.
  __VOID__,
  bool,
  i8,
  u8,
  i16,
  u16,
  i32,
  u32,
  i64,
  u64,
  f32,
  f64,
  isize,
  usize,
}

impl ToTokens for NumericArg {
  fn to_tokens(&self, tokens: &mut proc_macro2::TokenStream) {
    let ident = Ident::new(self.into(), Span::call_site());
    tokens.extend(quote! { #ident })
  }
}

#[derive(
  Copy, Clone, Debug, Eq, PartialEq, IntoStaticStr, EnumString, EnumIter,
)]
pub enum V8Arg {
  External,
  Object,
  Array,
  ArrayBuffer,
  ArrayBufferView,
  DataView,
  TypedArray,
  BigInt64Array,
  BigUint64Array,
  Float32Array,
  Float64Array,
  Int16Array,
  Int32Array,
  Int8Array,
  Uint16Array,
  Uint32Array,
  Uint8Array,
  Uint8ClampedArray,
  BigIntObject,
  BooleanObject,
  Date,
  Function,
  Map,
  NumberObject,
  Promise,
  PromiseResolver,
  Proxy,
  RegExp,
  Set,
  SharedArrayBuffer,
  StringObject,
  SymbolObject,
  WasmMemoryObject,
  WasmModuleObject,
  Primitive,
  BigInt,
  Boolean,
  Name,
  String,
  Symbol,
  Number,
  Integer,
  Int32,
  Uint32,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Special {
  HandleScope,
  OpState,
  String,
  CowStr,
  RefStr,
  FastApiCallbackOptions,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum RefType {
  Ref,
  Mut,
}

/// Args are not a 1:1 mapping with Rust types, rather they represent broad classes of types that
/// tend to have similar argument handling characteristics. This may need one more level of indirection
/// given how many of these types have option variants, however.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Arg {
  Void,
  Special(Special),
  Ref(RefType, Special),
  RcRefCell(Special),
  Option(Special),
  OptionNumeric(NumericArg),
  Slice(RefType, NumericArg),
  Ptr(RefType, NumericArg),
  OptionV8Local(V8Arg),
  V8Local(V8Arg),
  V8Global(V8Arg),
  V8Ref(RefType, V8Arg),
  Numeric(NumericArg),
  SerdeV8(String),
}

enum ParsedType {
  TSpecial(Special),
  TV8(V8Arg),
  TNumeric(NumericArg),
}

enum ParsedTypeContainer {
  CBare(ParsedType),
  COption(ParsedType),
  CRcRefCell(ParsedType),
  COptionV8Local(ParsedType),
  CV8Local(ParsedType),
  CV8Global(ParsedType),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RetVal {
  Infallible(Arg),
  Result(Arg),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsedSignature {
  // The parsed arguments
  pub args: Vec<Arg>,
  // The argument names
  pub names: Vec<String>,
  // The parsed return value
  pub ret_val: RetVal,
  // One and only one lifetime allowed
  pub lifetime: Option<String>,
  // Generic bounds: each generic must have one and only simple trait bound
  pub generic_bounds: BTreeMap<String, String>,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum AttributeModifier {
  /// #[serde], for serde_v8 types.
  Serde,
  /// #[smi], for small integers
  Smi,
  /// #[string], for strings.
  String,
}

#[derive(Error, Debug)]
pub enum SignatureError {
  #[error("Invalid argument: '{0}'")]
  ArgError(String, #[source] ArgError),
  #[error("Invalid return type")]
  RetError(#[from] ArgError),
  #[error("Only one lifetime is permitted")]
  TooManyLifetimes,
  #[error("Generic '{0}' must have one and only bound (either <T> and 'where T: Trait', or <T: Trait>)")]
  GenericBoundCardinality(String),
  #[error("Where clause predicate '{0}' (eg: where T: Trait) must appear in generics list (eg: <T>)")]
  WherePredicateMustAppearInGenerics(String),
  #[error("All generics must appear only once in the generics parameter list or where clause")]
  DuplicateGeneric(String),
  #[error("Generic lifetime '{0}' may not have bounds (eg: <'a: 'b>)")]
  LifetimesMayNotHaveBounds(String),
  #[error("Invalid generic: '{0}' Only simple generics bounds are allowed (eg: T: Trait)")]
  InvalidGeneric(String),
  #[error("Invalid predicate: '{0}' Only simple where predicates are allowed (eg: T: Trait)")]
  InvalidWherePredicate(String),
}

#[derive(Error, Debug)]
pub enum ArgError {
  #[error("Invalid self argument")]
  InvalidSelf,
  #[error("Invalid argument type: {0}")]
  InvalidType(String),
  #[error(
    "Invalid argument type path (should this be #[smi] or #[serde]?): {0}"
  )]
  InvalidTypePath(String),
  #[error("Too many attributes")]
  TooManyAttributes,
  #[error("The type {0} cannot be a reference")]
  InvalidReference(String),
  #[error("The type {0} must be a reference")]
  MissingReference(String),
  #[error("Invalid #[serde] type: {0}")]
  InvalidSerdeType(String),
  #[error("Invalid #[string] type: {0}")]
  InvalidStringType(String),
  #[error("Cannot use #[serde] for type: {0}")]
  InvalidSerdeAttributeType(String),
  #[error("Invalid v8 type: {0}")]
  InvalidV8Type(String),
  #[error("Internal error: {0}")]
  InternalError(String),
  #[error("Missing a #[string] attribute")]
  MissingStringAttribute,
}

#[derive(Copy, Clone, Default)]
struct Attributes {
  primary: Option<AttributeModifier>,
}

fn stringify_token(tokens: impl ToTokens) -> String {
  tokens
    .into_token_stream()
    .into_iter()
    .map(|s| s.to_string())
    .collect::<Vec<_>>()
    .join("")
}

pub fn parse_signature(
  attributes: Vec<Attribute>,
  signature: Signature,
) -> Result<ParsedSignature, SignatureError> {
  let mut args = vec![];
  let mut names = vec![];
  for input in signature.inputs {
    let name = match &input {
      FnArg::Receiver(_) => "self".to_owned(),
      FnArg::Typed(ty) => match &*ty.pat {
        Pat::Ident(ident) => ident.ident.to_string(),
        _ => "(complex)".to_owned(),
      },
    };
    names.push(name.clone());
    args.push(
      parse_arg(input).map_err(|err| SignatureError::ArgError(name, err))?,
    );
  }
  let ret_val =
    parse_return(parse_attributes(&attributes)?, &signature.output)?;
  let lifetime = parse_lifetime(&signature.generics)?;
  let generic_bounds = parse_generics(&signature.generics)?;
  Ok(ParsedSignature {
    args,
    names,
    ret_val,
    lifetime,
    generic_bounds,
  })
}

/// Extract one lifetime from the [`syn2::Generics`], ensuring that the lifetime is valid
/// and has no bounds.
fn parse_lifetime(
  generics: &Generics,
) -> Result<Option<String>, SignatureError> {
  let mut res = None;
  for param in &generics.params {
    if let GenericParam::Lifetime(lt) = param {
      if !lt.bounds.is_empty() {
        return Err(SignatureError::LifetimesMayNotHaveBounds(
          lt.lifetime.to_string(),
        ));
      }
      if res.is_some() {
        return Err(SignatureError::TooManyLifetimes);
      }
      res = Some(lt.lifetime.ident.to_string());
    }
  }
  Ok(res)
}

/// Parse and validate generics. We require one and only one trait bound for each generic
/// parameter. Tries to sanity check and return reasonable errors for possible signature errors.
fn parse_generics(
  generics: &Generics,
) -> Result<BTreeMap<String, String>, SignatureError> {
  let mut where_clauses = BTreeMap::new();

  // First, extract the where clause so we can detect duplicated predicates
  if let Some(where_clause) = &generics.where_clause {
    for predicate in &where_clause.predicates {
      let predicate = predicate.to_token_stream();
      let (generic_name, bound) = std::panic::catch_unwind(|| {
        use syn2 as syn;
        rules!(predicate => {
          ($t:ident : $bound:path) => (t.to_string(), stringify_token(bound)),
        })
      })
      .map_err(|_| {
        SignatureError::InvalidWherePredicate(predicate.to_string())
      })?;
      if where_clauses.insert(generic_name.clone(), bound).is_some() {
        return Err(SignatureError::DuplicateGeneric(generic_name));
      }
    }
  }

  let mut res = BTreeMap::new();
  for param in &generics.params {
    if let GenericParam::Type(ty) = param {
      let ty = ty.to_token_stream();
      let (name, bound) = std::panic::catch_unwind(|| {
        use syn2 as syn;
        rules!(ty => {
          ($t:ident : $bound:path) => (t.to_string(), Some(stringify_token(bound))),
          ($t:ident) => (t.to_string(), None),
        })
      }).map_err(|_| SignatureError::InvalidGeneric(ty.to_string()))?;
      let bound = match bound {
        Some(bound) => {
          if where_clauses.contains_key(&name) {
            return Err(SignatureError::GenericBoundCardinality(name));
          }
          bound
        }
        None => {
          let Some(bound) = where_clauses.remove(&name) else {
            return Err(SignatureError::GenericBoundCardinality(name));
          };
          bound
        }
      };
      if res.contains_key(&name) {
        return Err(SignatureError::DuplicateGeneric(name));
      }
      res.insert(name, bound);
    }
  }
  if !where_clauses.is_empty() {
    return Err(SignatureError::WherePredicateMustAppearInGenerics(
      where_clauses.into_keys().next().unwrap(),
    ));
  }

  Ok(res)
}

fn parse_attributes(attributes: &[Attribute]) -> Result<Attributes, ArgError> {
  let attrs = attributes
    .iter()
    .filter_map(parse_attribute)
    .collect::<Vec<_>>();

  if attrs.is_empty() {
    return Ok(Attributes::default());
  }
  if attrs.len() > 1 {
    return Err(ArgError::TooManyAttributes);
  }
  Ok(Attributes {
    primary: Some(*attrs.get(0).unwrap()),
  })
}

pub fn is_attribute_special(attr: &Attribute) -> bool {
  parse_attribute(attr).is_some()
}

fn parse_attribute(attr: &Attribute) -> Option<AttributeModifier> {
  let tokens = attr.into_token_stream();
  use syn2 as syn;
  std::panic::catch_unwind(|| {
    rules!(tokens => {
      (#[serde]) => Some(AttributeModifier::Serde),
      (#[smi]) => Some(AttributeModifier::Smi),
      (#[string]) => Some(AttributeModifier::String),
      (#[$_attr:meta]) => None,
    })
  })
  .expect("Failed to parse an attribute")
}

fn parse_return(
  attrs: Attributes,
  rt: &ReturnType,
) -> Result<RetVal, ArgError> {
  match rt {
    ReturnType::Default => Ok(RetVal::Infallible(Arg::Void)),
    ReturnType::Type(_, ty) => {
      let s = stringify_token(ty);
      let tokens = ty.into_token_stream();
      use syn2 as syn;

      std::panic::catch_unwind(|| {
        rules!(tokens => {
          // x::y::Result<Value>, like io::Result and other specialty result types
          ($($_package:ident ::)* Result < $ty:ty >) => {
            Ok(RetVal::Result(parse_type(attrs, &ty)?))
          }
          // x::y::Result<Value, Error>
          ($($_package:ident ::)* Result < $ty:ty, $_error:ty >) => {
            Ok(RetVal::Result(parse_type(attrs, &ty)?))
          }
          ($ty:ty) => {
            Ok(RetVal::Infallible(parse_type(attrs, &ty)?))
          }
        })
      })
      .map_err(|e| {
        ArgError::InternalError(format!(
          "parse_return({}) {}",
          s,
          e.downcast::<&str>().unwrap_or_default()
        ))
      })?
    }
  }
}

/// Parse a raw type into a container + type, allowing us to simplify the typechecks elsewhere in
/// this code.
fn parse_type_path(
  attrs: Attributes,
  is_ref: bool,
  tp: &TypePath,
) -> Result<ParsedTypeContainer, ArgError> {
  use ParsedType::*;
  use ParsedTypeContainer::*;

  if tp.path.segments.len() == 1 {
    let segment = tp.path.segments.first().unwrap().ident.to_string();
    for numeric in NumericArg::iter() {
      if Into::<&'static str>::into(numeric) == segment.as_str() {
        return Ok(CBare(TNumeric(numeric)));
      }
    }
  }

  use syn2 as syn;

  let tokens = tp.clone().into_token_stream();
  let res = std::panic::catch_unwind(|| {
    rules!(tokens => {
      ( $( std :: str  :: )? String ) => {
        Ok(CBare(TSpecial(Special::String)))
      }
      // Note that the reference is checked below
      ( $( std :: str  :: )? str ) => {
        Ok(CBare(TSpecial(Special::RefStr)))
      }
      ( $( std :: borrow :: )? Cow < str > ) => {
        Ok(CBare(TSpecial(Special::CowStr)))
      }
      ( $( std :: ffi :: )? c_void ) => Ok(CBare(TNumeric(NumericArg::__VOID__))),
      ( OpState ) => Ok(CBare(TSpecial(Special::OpState))),
      ( v8 :: HandleScope ) => Ok(CBare(TSpecial(Special::HandleScope))),
      ( v8 :: FastApiCallbackOptions ) => Ok(CBare(TSpecial(Special::FastApiCallbackOptions))),
      ( v8 :: Local < $( $_scope:lifetime , )? v8 :: $v8:ident >) => Ok(CV8Local(TV8(parse_v8_type(&v8)?))),
      ( v8 :: Global < $( $_scope:lifetime , )? v8 :: $v8:ident >) => Ok(CV8Global(TV8(parse_v8_type(&v8)?))),
      ( v8 :: $v8:ident ) => Ok(CBare(TV8(parse_v8_type(&v8)?))),
      ( Rc < RefCell < $ty:ty > > ) => Ok(CRcRefCell(TSpecial(parse_type_special(attrs, &ty)?))),
      ( Option < $ty:ty > ) => {
        match parse_type(attrs, &ty)? {
          Arg::Special(special) => Ok(COption(TSpecial(special))),
          Arg::Numeric(numeric) => Ok(COption(TNumeric(numeric))),
          Arg::V8Local(v8) => Ok(COptionV8Local(TV8(v8))),
          _ => Err(ArgError::InvalidType(stringify_token(ty)))
        }
      }
      ( $any:ty ) => Err(ArgError::InvalidTypePath(stringify_token(any))),
    })
  }).map_err(|e| ArgError::InternalError(format!("parse_type_path {e:?}")))??;

  // Ensure that we have the correct reference state. This is a bit awkward but it's
  // the easiest way to work with the 'rules!' macro above.
  match res {
    CBare(TSpecial(Special::RefStr | Special::OpState) | TV8(_)) => {
      if !is_ref {
        return Err(ArgError::MissingReference(stringify_token(tp)));
      }
    }
    _ => {
      if is_ref {
        return Err(ArgError::InvalidReference(stringify_token(tp)));
      }
    }
  }

  match res {
    CBare(TSpecial(Special::RefStr | Special::CowStr | Special::String)) => {
      if attrs.primary != Some(AttributeModifier::String) {
        return Err(ArgError::MissingStringAttribute);
      }
    }
    CBare(_) => {
      if attrs.primary == Some(AttributeModifier::String) {
        return Err(ArgError::InvalidStringType(stringify_token(tp)));
      }
    }
    _ => {
      // Ignore for other containers
    }
  }

  Ok(res)
}

fn parse_v8_type(v8: &Ident) -> Result<V8Arg, ArgError> {
  let v8 = v8.to_string();
  V8Arg::try_from(v8.as_str()).map_err(|_| ArgError::InvalidV8Type(v8))
}

fn parse_type_special(
  attrs: Attributes,
  ty: &Type,
) -> Result<Special, ArgError> {
  match parse_type(attrs, ty)? {
    Arg::Special(special) => Ok(special),
    _ => Err(ArgError::InvalidType(stringify_token(ty))),
  }
}

fn parse_type(attrs: Attributes, ty: &Type) -> Result<Arg, ArgError> {
  use ParsedType::*;
  use ParsedTypeContainer::*;

  if let Some(primary) = attrs.primary {
    match primary {
      AttributeModifier::Serde => match ty {
        Type::Path(of) => {
          // If this type will parse without #[serde], it is illegal to use this type with #[serde]
          if parse_type_path(Attributes::default(), false, of).is_ok() {
            return Err(ArgError::InvalidSerdeAttributeType(stringify_token(
              ty,
            )));
          }
          return Ok(Arg::SerdeV8(stringify_token(of.path.clone())));
        }
        _ => return Err(ArgError::InvalidSerdeType(stringify_token(ty))),
      },
      AttributeModifier::String => {
        // We handle this as part of the normal parsing process
      }
      AttributeModifier::Smi => {
        return Ok(Arg::Numeric(NumericArg::__SMI__));
      }
    }
  };
  match ty {
    Type::Tuple(of) => {
      if of.elems.is_empty() {
        Ok(Arg::Void)
      } else {
        Err(ArgError::InvalidType(stringify_token(ty)))
      }
    }
    Type::Reference(of) => {
      let mut_type = if of.mutability.is_some() {
        RefType::Mut
      } else {
        RefType::Ref
      };
      match &*of.elem {
        Type::Slice(of) => match parse_type(attrs, &of.elem)? {
          Arg::Numeric(numeric) => Ok(Arg::Slice(mut_type, numeric)),
          _ => Err(ArgError::InvalidType(stringify_token(ty))),
        },
        Type::Path(of) => match parse_type_path(attrs, true, of)? {
          CBare(TSpecial(Special::RefStr)) => Ok(Arg::Special(Special::RefStr)),
          COption(TSpecial(Special::RefStr)) => {
            Ok(Arg::Option(Special::RefStr))
          }
          CBare(TV8(v8)) => Ok(Arg::V8Ref(mut_type, v8)),
          CBare(TSpecial(special)) => Ok(Arg::Ref(mut_type, special)),
          _ => Err(ArgError::InvalidType(stringify_token(ty))),
        },
        _ => Err(ArgError::InvalidType(stringify_token(ty))),
      }
    }
    Type::Ptr(of) => {
      let mut_type = if of.mutability.is_some() {
        RefType::Mut
      } else {
        RefType::Ref
      };
      match &*of.elem {
        Type::Path(of) => match parse_type_path(attrs, false, of)? {
          CBare(TNumeric(numeric)) => Ok(Arg::Ptr(mut_type, numeric)),
          _ => Err(ArgError::InvalidType(stringify_token(ty))),
        },
        _ => Err(ArgError::InvalidType(stringify_token(ty))),
      }
    }
    Type::Path(of) => match parse_type_path(attrs, false, of)? {
      CBare(TNumeric(numeric)) => Ok(Arg::Numeric(numeric)),
      CBare(TSpecial(special)) => Ok(Arg::Special(special)),
      COption(TNumeric(special)) => Ok(Arg::OptionNumeric(special)),
      COption(TSpecial(special)) => Ok(Arg::Option(special)),
      CRcRefCell(TSpecial(special)) => Ok(Arg::RcRefCell(special)),
      COptionV8Local(TV8(v8)) => Ok(Arg::OptionV8Local(v8)),
      CV8Local(TV8(v8)) => Ok(Arg::V8Local(v8)),
      CV8Global(TV8(v8)) => Ok(Arg::V8Global(v8)),
      _ => Err(ArgError::InvalidType(stringify_token(ty))),
    },
    _ => Err(ArgError::InvalidType(stringify_token(ty))),
  }
}

fn parse_arg(arg: FnArg) -> Result<Arg, ArgError> {
  let FnArg::Typed(typed) = arg else {
    return Err(ArgError::InvalidSelf);
  };
  parse_type(parse_attributes(&typed.attrs)?, &typed.ty)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::op2::signature::parse_signature;
  use syn2::parse_str;
  use syn2::ItemFn;

  // We can't test pattern args :/
  // https://github.com/rust-lang/rfcs/issues/2688
  macro_rules! test {
    (
      // Function attributes
      $(# [ $fn_attr:ident ])?
      // fn name < 'scope, GENERIC1, GENERIC2, ... >
      fn $name:ident $( < $scope:lifetime $( , $generic:ident)* >)?
      (
        // Argument attribute, argument
        $( $(# [ $attr:ident ])? $ident:ident : $ty:ty ),*
      )
      // Return value
      $(-> $(# [ $ret_attr:ident ])? $ret:ty)?
      // Where clause
      $( where $($trait:ident : $bounds:path),* )?
      ;
      // Expected return value
      $( < $( $lifetime_res:lifetime )? $(, $generic_res:ident : $bounds_res:path )* >)? ( $( $arg_res:expr ),* ) -> $ret_res:expr ) => {
      #[test]
      fn $name() {
        test(
          stringify!($( #[$fn_attr] )? fn op $( < $scope $( , $generic)* >)? ( $( $( #[$attr] )? $ident : $ty ),* ) $(-> $( #[$ret_attr] )? $ret)? $( where $($trait : $bounds),* )? {}),
          stringify!($( < $( $lifetime_res )? $(, $generic_res : $bounds_res)* > )?),
          stringify!($($arg_res),*),
          stringify!($ret_res)
        );
      }
    };
  }

  fn test(
    op: &str,
    generics_expected: &str,
    args_expected: &str,
    return_expected: &str,
  ) {
    // Parse the provided macro input as an ItemFn
    let item_fn = parse_str::<ItemFn>(op)
      .unwrap_or_else(|_| panic!("Failed to parse {op} as a ItemFn"));

    let attrs = item_fn.attrs;
    let sig = parse_signature(attrs, item_fn.sig).unwrap_or_else(|err| {
      panic!("Failed to successfully parse signature from {op} ({err:?})")
    });
    println!("Raw parsed signatures = {sig:?}");

    let mut generics_res = vec![];
    if let Some(lifetime) = sig.lifetime {
      generics_res.push(format!("'{lifetime}"));
    }
    for (name, bounds) in sig.generic_bounds {
      generics_res.push(format!("{name} : {bounds}"));
    }
    if !generics_res.is_empty() {
      assert_eq!(
        generics_expected,
        format!("< {} >", generics_res.join(", "))
      );
    }
    assert_eq!(
      args_expected,
      format!("{:?}", sig.args).trim_matches(|c| c == '[' || c == ']')
    );
    assert_eq!(return_expected, format!("{:?}", sig.ret_val));
  }

  macro_rules! expect_fail {
    ($name:ident, $error:expr, $f:item) => {
      #[test]
      pub fn $name() {
        expect_fail(stringify!($f), stringify!($error));
      }
    };
  }

  fn expect_fail(op: &str, error: &str) {
    // Parse the provided macro input as an ItemFn
    let item_fn = parse_str::<ItemFn>(op)
      .unwrap_or_else(|_| panic!("Failed to parse {op} as a ItemFn"));
    let attrs = item_fn.attrs;
    let err = parse_signature(attrs, item_fn.sig)
      .expect_err("Expected function to fail to parse");
    assert_eq!(format!("{err:?}"), error.to_owned());
  }

  test!(
    fn op_state_and_number(opstate: &mut OpState, a: u32) -> ();
    (Ref(Mut, OpState), Numeric(u32)) -> Infallible(Void)
  );
  test!(
    fn op_slices(r#in: &[u8], out: &mut [u8]);
    (Slice(Ref, u8), Slice(Mut, u8)) -> Infallible(Void)
  );
  test!(
    #[serde] fn op_serde(#[serde] input: package::SerdeInputType) -> Result<package::SerdeReturnType, Error>;
    (SerdeV8("package::SerdeInputType")) -> Result(SerdeV8("package::SerdeReturnType"))
  );
  test!(
    fn op_local(input: v8::Local<v8::String>) -> Result<v8::Local<v8::String>, Error>;
    (V8Local(String)) -> Result(V8Local(String))
  );
  test!(
    fn op_resource(#[smi] rid: ResourceId, buffer: &[u8]);
    (Numeric(__SMI__), Slice(Ref, u8)) ->  Infallible(Void)
  );
  test!(
    fn op_option_numeric_result(state: &mut OpState) -> Result<Option<u32>, AnyError>;
    (Ref(Mut, OpState)) -> Result(OptionNumeric(u32))
  );
  test!(
    fn op_ffi_read_f64(state: &mut OpState, ptr: * mut c_void, offset: isize) -> Result <f64, AnyError>;
    (Ref(Mut, OpState), Ptr(Mut, __VOID__), Numeric(isize)) -> Result(Numeric(f64))
  );
  test!(
    fn op_print(#[string] msg: &str, is_err: bool) -> Result<(), Error>;
    (Special(RefStr), Numeric(bool)) -> Result(Void)
  );
  test!(
    #[string] fn op_lots_of_strings(#[string] s: String, #[string] s2: Option<String>, #[string] s3: Cow<str>) -> String;
    (Special(String), Option(String), Special(CowStr)) -> Infallible(Special(String))
  );
  test!(
    #[string] fn op_lots_of_option_strings(#[string] s: Option<String>, #[string] s2: Option<&str>, #[string] s3: Option<Cow<str>>) -> Option<String>;
    (Option(String), Option(RefStr), Option(CowStr)) -> Infallible(Option(String))
  );
  test!(
    fn op_scope<'s>(#[string] msg: &'s str);
    <'s> (Special(RefStr)) -> Infallible(Void)
  );
  test!(
    fn op_scope_and_generics<'s, AB, BC>(#[string] msg: &'s str) where AB: some::Trait, BC: OtherTrait;
    <'s, AB: some::Trait, BC: OtherTrait> (Special(RefStr)) -> Infallible(Void)
  );
  test!(
    fn op_v8_types(s: &mut v8::String, s2: v8::Local<v8::String>, s3: v8::Global<v8::String>);
    (V8Ref(Mut, String), V8Local(String), V8Global(String)) -> Infallible(Void)
  );

  // Args

  expect_fail!(
    op_with_bad_string1,
    ArgError("s", MissingStringAttribute),
    fn f(s: &str) {}
  );
  expect_fail!(
    op_with_bad_string2,
    ArgError("s", MissingStringAttribute),
    fn f(s: String) {}
  );
  expect_fail!(
    op_with_bad_string3,
    ArgError("s", MissingStringAttribute),
    fn f(s: Cow<str>) {}
  );

  // Generics

  expect_fail!(op_with_two_lifetimes, TooManyLifetimes, fn f<'a, 'b>() {});
  expect_fail!(
    op_with_lifetime_bounds,
    LifetimesMayNotHaveBounds("'a"),
    fn f<'a: 'b, 'b>() {}
  );
  expect_fail!(
    op_with_missing_bounds,
    GenericBoundCardinality("B"),
    fn f<'a, B>() {}
  );
  expect_fail!(
    op_with_duplicate_bounds,
    GenericBoundCardinality("B"),
    fn f<'a, B: Trait>()
    where
      B: Trait,
    {
    }
  );
  expect_fail!(
    op_with_extra_bounds,
    WherePredicateMustAppearInGenerics("C"),
    fn f<'a, B>()
    where
      B: Trait,
      C: Trait,
    {
    }
  );

  #[test]
  fn test_parse_result() {
    let rt = parse_str::<ReturnType>("-> Result < (), Error >")
      .expect("Failed to parse");
    println!("{:?}", parse_return(Attributes::default(), &rt));
  }
}
