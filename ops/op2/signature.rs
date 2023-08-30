// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use deno_proc_macro_rules::rules;
use proc_macro2::Ident;
use proc_macro2::Span;
use proc_macro2::TokenStream;
use quote::format_ident;
use quote::quote;
use quote::ToTokens;
use quote::TokenStreamExt;
use std::collections::BTreeMap;
use strum::IntoEnumIterator;
use strum::IntoStaticStr;
use strum_macros::EnumIter;
use strum_macros::EnumString;
use syn::Attribute;
use syn::FnArg;
use syn::GenericParam;
use syn::Generics;
use syn::Pat;
use syn::Path;
use syn::Signature;
use syn::Type;
use syn::TypeParamBound;
use syn::TypePath;
use thiserror::Error;

use super::signature_retval::parse_return;

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

impl NumericArg {
  /// Returns the primary mapping from this primitive to an associated V8 typed array.
  pub fn v8_array_type(self) -> Option<V8Arg> {
    use NumericArg::*;
    use V8Arg::*;
    Some(match self {
      i8 => Int8Array,
      u8 => Uint8Array,
      i16 => Int16Array,
      u16 => Uint16Array,
      i32 => Int32Array,
      u32 => Uint32Array,
      i64 => BigInt64Array,
      u64 => BigUint64Array,
      f32 => Float32Array,
      f64 => Float64Array,
      _ => return None,
    })
  }
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
  Value,
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

impl ToTokens for V8Arg {
  fn to_tokens(&self, tokens: &mut TokenStream) {
    let v8: &'static str = self.into();
    tokens.append(format_ident!("{v8}"))
  }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Special {
  HandleScope,
  OpState,
  FastApiCallbackOptions,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Strings {
  String,
  CowStr,
  RefStr,
  CowByte,
}

/// Buffers are complicated and may be shared/owned, shared/unowned, a copy, or detached.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Buffer {
  /// Shared/unowned, may be resizable. [`&[u8]`], [`&mut [u8]`], [`&[u32]`], etc...
  Slice(RefType, NumericArg),
  /// Shared/unowned, may be resizable. [`*const u8`], [`*mut u8`], [`*const u32`], etc...
  Ptr(RefType, NumericArg),
  /// Owned, copy. [`Box<[u8]>`], [`Box<[u32]>`], etc...
  BoxSlice(NumericArg),
  /// Owned, copy. [`Vec<u8>`], [`Vec<u32>`], etc...
  Vec(NumericArg),
  /// Maybe shared or a copy. Stored in `bytes::Bytes`
  Bytes(BufferMode),
  /// Owned, copy. Stored in `bytes::BytesMut`
  BytesMut(BufferMode),
  /// Shared, not resizable (or resizable and detatched), stored in `serde_v8::V8Slice`
  V8Slice(BufferMode),
  /// Shared, not resizable (or resizable and detatched), stored in `serde_v8::JsBuffer`
  JsBuffer(BufferMode),
}

impl Buffer {
  const fn valid_modes(
    &self,
    position: Position,
  ) -> &'static [AttributeModifier] {
    use AttributeModifier::Buffer as B;
    use Buffer::*;
    use BufferMode::*;
    match position {
      Position::Arg => match self {
        Bytes(..) | BytesMut(..) | Vec(..) | BoxSlice(..) => &[B(Copy)],
        JsBuffer(..) | V8Slice(..) => &[B(Copy), B(Detach), B(Default)],
        Slice(..) | Ptr(..) => &[B(Default)],
      },
      Position::RetVal => match self {
        Bytes(..) | BytesMut(..) | JsBuffer(..) | V8Slice(..) | Vec(..)
        | BoxSlice(..) => &[B(Default)],
        Slice(..) | Ptr(..) => &([]),
      },
    }
  }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum External {
  /// c_void
  Ptr(RefType),
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
  String(Strings),
  Buffer(Buffer),
  External(External),
  Ref(RefType, Special),
  RcRefCell(Special),
  Option(Special),
  OptionString(Strings),
  OptionNumeric(NumericArg),
  OptionV8Local(V8Arg),
  OptionV8Global(V8Arg),
  V8Local(V8Arg),
  V8Global(V8Arg),
  OptionV8Ref(RefType, V8Arg),
  V8Ref(RefType, V8Arg),
  Numeric(NumericArg),
  SerdeV8(String),
  State(RefType, String),
  OptionState(RefType, String),
}

impl Arg {
  /// Is this argument virtual? ie: does it come from the æther rather than a concrete JavaScript input
  /// argument?
  #[allow(clippy::match_like_matches_macro)]
  pub const fn is_virtual(&self) -> bool {
    match self {
      Self::Special(
        Special::FastApiCallbackOptions
        | Special::OpState
        | Special::HandleScope,
      ) => true,
      Self::Ref(
        _,
        Special::FastApiCallbackOptions
        | Special::OpState
        | Special::HandleScope,
      ) => true,
      Self::RcRefCell(
        Special::FastApiCallbackOptions
        | Special::OpState
        | Special::HandleScope,
      ) => true,
      Self::State(..) | Self::OptionState(..) => true,
      _ => false,
    }
  }

  /// Convert the [`Arg`] into a [`TokenStream`] representing the fully-qualified type.
  #[allow(unused)] // unused for now but keeping
  pub fn type_token(&self, deno_core: &TokenStream) -> TokenStream {
    match self {
      Arg::V8Ref(RefType::Ref, v8) => quote!(&#deno_core::v8::#v8),
      Arg::V8Ref(RefType::Mut, v8) => quote!(&mut #deno_core::v8::#v8),
      Arg::V8Local(v8) => quote!(#deno_core::v8::Local<#deno_core::v8::#v8>),
      Arg::V8Global(v8) => quote!(#deno_core::v8::Global<#deno_core::v8::#v8>),
      Arg::OptionV8Ref(RefType::Ref, v8) => {
        quote!(::std::option::Option<&#deno_core::v8::#v8>)
      }
      Arg::OptionV8Ref(RefType::Mut, v8) => {
        quote!(::std::option::Option<&mut #deno_core::v8::#v8>)
      }
      Arg::OptionV8Local(v8) => {
        quote!(::std::option::Option<#deno_core::v8::Local<#deno_core::v8::#v8>>)
      }
      Arg::OptionV8Global(v8) => {
        quote!(::std::option::Option<#deno_core::v8::Global<#deno_core::v8::#v8>>)
      }
      _ => todo!(),
    }
  }

  /// Is this type an [`Option`]?
  pub const fn is_option(&self) -> bool {
    matches!(
      self,
      Arg::OptionV8Ref(..)
        | Arg::OptionV8Local(..)
        | Arg::OptionV8Global(..)
        | Arg::OptionNumeric(..)
        | Arg::Option(..)
        | Arg::OptionString(..)
        | Arg::OptionState(..)
    )
  }

  /// Return the `Some` part of this `Option` type, or `None` if it is not an `Option`.
  pub fn some_type(&self) -> Option<Arg> {
    Some(match self {
      Arg::OptionV8Ref(r, t) => Arg::V8Ref(*r, *t),
      Arg::OptionV8Local(t) => Arg::V8Local(*t),
      Arg::OptionV8Global(t) => Arg::V8Global(*t),
      Arg::OptionNumeric(t) => Arg::Numeric(*t),
      Arg::Option(t) => Arg::Special(*t),
      Arg::OptionString(t) => Arg::String(*t),
      Arg::OptionState(r, t) => Arg::State(*r, t.clone()),
      _ => return None,
    })
  }

  /// This must be kept in sync with the `RustToV8`/`RustToV8Fallible` implementations in `deno_core`. If
  /// this falls out of sync, you will see compile errors.
  pub fn slow_retval(&self) -> ArgSlowRetval {
    if let Some(some) = self.some_type() {
      // If this is an optional return value, we use the same return type as the underlying object.
      match some.slow_retval() {
        // We need a scope in the case of an option so we can allocate a null
        ArgSlowRetval::V8LocalNoScope => ArgSlowRetval::RetVal,
        rv => rv,
      }
    } else {
      match self {
        Arg::Numeric(
          NumericArg::i64
          | NumericArg::u64
          | NumericArg::isize
          | NumericArg::usize,
        ) => ArgSlowRetval::V8Local,
        Arg::Void | Arg::Numeric(_) => ArgSlowRetval::RetVal,
        Arg::External(_) => ArgSlowRetval::V8Local,
        // Fast return value path for empty strings
        Arg::String(_) => ArgSlowRetval::RetValFallible,
        Arg::SerdeV8(_) => ArgSlowRetval::V8LocalFalliable,
        // No scope required for these
        Arg::V8Local(_) => ArgSlowRetval::V8LocalNoScope,
        Arg::V8Global(_) => ArgSlowRetval::V8Local,
        Arg::Buffer(
          Buffer::JsBuffer(BufferMode::Default)
          | Buffer::Vec(NumericArg::u8)
          | Buffer::BoxSlice(NumericArg::u8)
          | Buffer::BytesMut(BufferMode::Default),
        ) => ArgSlowRetval::V8LocalFalliable,
        _ => ArgSlowRetval::None,
      }
    }
  }

  /// Does this type have a marker (used for specialization of serialization/deserialization)?
  pub fn marker(&self) -> ArgMarker {
    match self {
      Arg::SerdeV8(_) => ArgMarker::Serde,
      Arg::Numeric(NumericArg::__SMI__) => ArgMarker::Smi,
      _ => ArgMarker::None,
    }
  }
}

#[derive(PartialEq, Eq)]
/// How can this argument be represented?
pub enum ArgSlowRetval {
  /// The argument is not supported in the return position.
  None,
  /// The argument is supported as a fast path in `v8::ReturnValue`. Implies that there is also
  /// a `V8Local` implementation in cases where there is no [`v8::ReturnValue`]. Does not require
  /// a scope.
  RetVal,
  /// Like `RetVal`, but fallible. Unlike `RetVal`, requires a scope.
  RetValFallible,
  /// The argument is only supported as a `v8::Local`, and it may not fail (eg: integers, floats).
  V8Local,
  /// The argument is only supported as a `v8::Local`, and it does not allocate (ie: it is already
  /// a `v8::Local`).
  V8LocalNoScope,
  /// The argument is only supported as a `v8::Local`, and it may fail (eg: strings, arrays).
  V8LocalFalliable,
}

/// Specifies an ArgMarker wrapper for a type used for trait-based serialization.
pub enum ArgMarker {
  None,
  /// This type should be serialized with serde_v8.
  Serde,
  /// This type should be serialized as an SMI.
  Smi,
}

pub enum ParsedType {
  TSpecial(Special),
  TString(Strings),
  TBuffer(Buffer),
  TV8(V8Arg),
  // TODO(mmastrac): We need to carry the mut status through somehow
  TV8Mut(V8Arg),
  TNumeric(NumericArg),
}

impl ParsedType {
  /// Returns the valid attributes for this particular type, `None` if no attributes are valid and
  /// `Some([])` if the type is not valid in this position.
  fn required_attributes(
    &self,
    position: Position,
  ) -> Option<&'static [AttributeModifier]> {
    use ParsedType::*;
    match self {
      TNumeric(
        NumericArg::u64
        | NumericArg::i64
        | NumericArg::usize
        | NumericArg::isize,
      ) => Some(&[AttributeModifier::Bigint]),
      TBuffer(buffer) => Some(buffer.valid_modes(position)),
      TString(Strings::CowByte) => {
        Some(&[AttributeModifier::String(StringMode::OneByte)])
      }
      TString(..) => Some(&[AttributeModifier::String(StringMode::Default)]),
      _ => None,
    }
  }
}

pub enum ParsedTypeContainer {
  CBare(ParsedType),
  COption(ParsedType),
  CRcRefCell(ParsedType),
  COptionV8Local(ParsedType),
  COptionV8Global(ParsedType),
  CV8Local(ParsedType),
  CV8Global(ParsedType),
}

impl ParsedTypeContainer {
  /// Returns the valid attributes for this particular type, `None` if no attributes are valid and
  /// `Some([])` if the type is not valid in this position.
  pub fn required_attributes(
    &self,
    position: Position,
  ) -> Option<&'static [AttributeModifier]> {
    use ParsedTypeContainer::*;
    match self {
      CV8Local(_) | COptionV8Local(_) => None,
      CV8Global(_) | COptionV8Global(_) => Some(&[AttributeModifier::Global]),
      CBare(t) | COption(t) | CRcRefCell(t) => t.required_attributes(position),
    }
  }

  fn validate_attributes(
    &self,
    position: Position,
    attrs: Attributes,
    tp: &impl ToTokens,
  ) -> Result<(), ArgError> {
    match self.required_attributes(position) {
      None => match attrs.primary {
        None => {}
        Some(attr) => {
          return Err(ArgError::InvalidAttributeType(
            attr.name(),
            stringify_token(tp),
          ))
        }
      },
      Some(attr) => {
        if attr.is_empty() {
          return Err(ArgError::NotAllowedInThisPosition(stringify_token(tp)));
        }
        match attrs.primary {
          None => {
            return Err(ArgError::MissingAttribute(
              attr[0].name(),
              stringify_token(tp),
            ))
          }
          Some(primary) => {
            if !attr.contains(&primary) {
              return Err(ArgError::MissingAttribute(
                attr[0].name(),
                stringify_token(tp),
              ));
            }
          }
        }
      }
    };
    Ok(())
  }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RetVal {
  /// An op that can never fail.
  Infallible(Arg),
  /// An op returning Result<Something, ...>
  Result(Arg),
  /// An op returning a future, either `async fn() -> Something` or `fn() -> impl Future<Output = Something>`.
  Future(Arg),
  /// An op returning a future with a result, either `async fn() -> Result<Something, ...>`
  /// or `fn() -> impl Future<Output = Result<Something, ...>>`.
  FutureResult(Arg),
  /// An op returning a result future: `fn() -> Result<impl Future<Output = Something>>`,
  /// allowing it to exit before starting any async work.
  ResultFuture(Arg),
  /// An op returning a result future of a result: `fn() -> Result<impl Future<Output = Result<Something, ...>>>`,
  /// allowing it to exit before starting any async work.
  ResultFutureResult(Arg),
}

impl RetVal {
  pub fn is_async(&self) -> bool {
    use RetVal::*;
    matches!(
      self,
      Future(..) | FutureResult(..) | ResultFuture(..) | ResultFutureResult(..)
    )
  }
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
pub enum StringMode {
  /// Default mode.
  Default,
  /// One-byte strings (aka Latin-1).
  OneByte,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum BufferMode {
  /// Default mode.
  Default,
  /// Unsafely shared buffers that may possibly change on the JavaScript side upon re-entry into
  /// V8. Rust code should not treat these as traditional buffers.
  Unsafe,
  /// Shared buffers that are copied from V8 unconditionally. May be expensive, but these
  /// buffers are guaranteed to be owned by Rust.
  Copy,
  /// Buffers that are detached and owned purely by Rust. JavaScript will no longer have
  /// access to these buffers and will see zero-sized buffers rather than the contents
  /// that were passed in here.
  Detach,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum AttributeModifier {
  /// #[serde], for serde_v8 types.
  Serde,
  /// #[smi], for non-integral ID types representing small integers (-2³¹ and 2³¹-1 on 64-bit platforms,
  /// see https://medium.com/fhinkel/v8-internals-how-small-is-a-small-integer-e0badc18b6da).
  Smi,
  /// #[string], for strings.
  String(StringMode),
  /// #[state], for automatic OpState extraction.
  State,
  /// #[buffer], for buffers.
  Buffer(BufferMode),
  /// #[global], for [`v8::Global`]s
  Global,
  /// #[bigint], for u64/usize/i64/isize
  Bigint,
}

impl AttributeModifier {
  fn name(&self) -> &'static str {
    match self {
      AttributeModifier::Bigint => "bigint",
      AttributeModifier::Buffer(_) => "buffer",
      AttributeModifier::Smi => "smi",
      AttributeModifier::Serde => "serde",
      AttributeModifier::String(_) => "string",
      AttributeModifier::State => "state",
      AttributeModifier::Global => "global",
    }
  }
}

#[derive(Error, Debug)]
pub enum SignatureError {
  #[error("Invalid argument: '{0}'")]
  ArgError(String, #[source] ArgError),
  #[error("Invalid return type")]
  RetError(#[from] RetError),
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
  #[error("State may be either a single OpState parameter, one mutable #[state], or multiple immultiple #[state]s")]
  InvalidOpStateCombination,
}

#[derive(Error, Debug)]
pub enum AttributeError {
  #[error("Unknown or invalid attribute '{0}'")]
  InvalidAttribute(String),
  #[error("Too many attributes")]
  TooManyAttributes,
}

#[derive(Error, Debug)]
pub enum ArgError {
  #[error("Invalid self argument")]
  InvalidSelf,
  #[error("Invalid argument type: {0} ({1})")]
  InvalidType(String, &'static str),
  #[error("Invalid numeric argument type: {0}")]
  InvalidNumericType(String),
  #[error(
    "Invalid argument type path (should this be #[smi] or #[serde]?): {0}"
  )]
  InvalidTypePath(String),
  #[error("The type {0} cannot be a reference")]
  InvalidReference(String),
  #[error("The type {0} must be a reference")]
  MissingReference(String),
  #[error("Invalid or deprecated #[serde] type '{0}': {1}")]
  InvalidSerdeType(String, &'static str),
  #[error("Invalid #[{0}] for type: {1}")]
  InvalidAttributeType(&'static str, String),
  #[error("Cannot use #[serde] for type: {0}")]
  InvalidSerdeAttributeType(String),
  #[error("Invalid v8 type: {0}")]
  InvalidV8Type(String),
  #[error("Internal error: {0}")]
  InternalError(String),
  #[error("Missing a #[{0}] attribute for type: {1}")]
  MissingAttribute(&'static str, String),
  #[error("Invalid #[state] type '{0}'")]
  InvalidStateType(String),
  #[error("Argument attribute error")]
  AttributeError(#[from] AttributeError),
  #[error("The type '{0}' is not allowed in this position")]
  NotAllowedInThisPosition(String),
}

#[derive(Error, Debug)]
pub enum RetError {
  #[error("Invalid return type")]
  InvalidType(#[from] ArgError),
  #[error("Return value attribute error")]
  AttributeError(#[from] AttributeError),
}

#[derive(Copy, Clone, Default)]
pub(crate) struct Attributes {
  primary: Option<AttributeModifier>,
}

/// Where is this type defined?
#[derive(Copy, Clone, PartialEq, Eq)]
pub enum Position {
  /// Argument
  Arg,
  /// Return value
  RetVal,
}

impl Attributes {
  pub fn string() -> Self {
    Self {
      primary: Some(AttributeModifier::String(StringMode::Default)),
    }
  }
}

pub(crate) fn stringify_token(tokens: impl ToTokens) -> String {
  tokens
    .into_token_stream()
    .into_iter()
    .map(|s| s.to_string())
    .collect::<Vec<_>>()
    .join("")
    // Ick.
    // TODO(mmastrac): Should we pretty-format this instead?
    .replace(" , ", ", ")
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
  let ret_val = parse_return(
    signature.asyncness.is_some(),
    parse_attributes(&attributes).map_err(RetError::AttributeError)?,
    &signature.output,
  )?;
  let lifetime = parse_lifetime(&signature.generics)?;
  let generic_bounds = parse_generics(&signature.generics)?;

  let mut has_opstate = false;
  let mut has_mut_state = false;
  let mut has_ref_state = false;

  for arg in &args {
    match arg {
      Arg::RcRefCell(Special::OpState) | Arg::Ref(_, Special::OpState) => {
        has_opstate = true
      }
      Arg::State(RefType::Ref, _) | Arg::OptionState(RefType::Ref, _) => {
        has_ref_state = true
      }
      Arg::State(RefType::Mut, _) | Arg::OptionState(RefType::Mut, _) => {
        if has_mut_state {
          return Err(SignatureError::InvalidOpStateCombination);
        }
        has_mut_state = true;
      }
      _ => {}
    }
  }

  // Ensure that either zero or one and only one of these are true
  if has_opstate as u8 + has_mut_state as u8 + has_ref_state as u8 > 1 {
    return Err(SignatureError::InvalidOpStateCombination);
  }

  Ok(ParsedSignature {
    args,
    names,
    ret_val,
    lifetime,
    generic_bounds,
  })
}

/// Extract one lifetime from the [`syn::Generics`], ensuring that the lifetime is valid
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

/// Parse a bound as a string. Valid bounds include "Trait" and "Trait + 'static". All
/// other bounds are invalid.
fn parse_bound(bound: &Type) -> Result<String, SignatureError> {
  let error = || {
    Err(SignatureError::InvalidWherePredicate(stringify_token(
      bound,
    )))
  };

  Ok(match bound {
    Type::TraitObject(t) => {
      let mut has_static_lifetime = false;
      let mut bound = None;
      for b in &t.bounds {
        match b {
          TypeParamBound::Lifetime(lt) => {
            if lt.ident != "static" || has_static_lifetime {
              return error();
            }
            has_static_lifetime = true;
          }
          TypeParamBound::Trait(t) => {
            if bound.is_some() {
              return error();
            }
            bound = Some(stringify_token(t));
          }
          _ => return error(),
        }
      }
      let Some(bound) = bound else {
        return error();
      };
      if has_static_lifetime {
        format!("{bound} + 'static")
      } else {
        bound
      }
    }
    Type::Path(p) => stringify_token(p),
    _ => {
      return error();
    }
  })
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
        rules!(predicate => {
          ($t:ident : $bound:ty) => (t.to_string(), bound),
        })
      })
      .map_err(|_| {
        SignatureError::InvalidWherePredicate(predicate.to_string())
      })?;
      let bound = parse_bound(&bound)?;
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
        rules!(ty => {
          ($t:ident : $bound:ty) => (t.to_string(), Some(bound)),
          ($t:ident) => (t.to_string(), None),
        })
      })
      .map_err(|_| SignatureError::InvalidGeneric(ty.to_string()))?;
      let bound = match bound {
        Some(bound) => {
          if where_clauses.contains_key(&name) {
            return Err(SignatureError::GenericBoundCardinality(name));
          }
          parse_bound(&bound)?
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

fn parse_attributes(
  attributes: &[Attribute],
) -> Result<Attributes, AttributeError> {
  let mut attrs = vec![];
  for attr in attributes {
    if let Some(attr) = parse_attribute(attr)? {
      attrs.push(attr)
    }
  }

  if attrs.is_empty() {
    return Ok(Attributes::default());
  }
  if attrs.len() > 1 {
    return Err(AttributeError::TooManyAttributes);
  }
  Ok(Attributes {
    primary: Some(*attrs.get(0).unwrap()),
  })
}

/// Is this a special attribute that we understand?
pub fn is_attribute_special(attr: &Attribute) -> bool {
  parse_attribute(attr).unwrap_or_default().is_some()
}

/// Parses an attribute, returning None if this is an attribute we support but is
/// otherwise unknown (ie: doc comments).
fn parse_attribute(
  attr: &Attribute,
) -> Result<Option<AttributeModifier>, AttributeError> {
  let tokens = attr.into_token_stream();
  let res = std::panic::catch_unwind(|| {
    rules!(tokens => {
      (#[bigint]) => Some(AttributeModifier::Bigint),
      (#[serde]) => Some(AttributeModifier::Serde),
      (#[smi]) => Some(AttributeModifier::Smi),
      (#[string]) => Some(AttributeModifier::String(StringMode::Default)),
      (#[string(onebyte)]) => Some(AttributeModifier::String(StringMode::OneByte)),
      (#[state]) => Some(AttributeModifier::State),
      (#[buffer]) => Some(AttributeModifier::Buffer(BufferMode::Default)),
      (#[buffer(unsafe)]) => Some(AttributeModifier::Buffer(BufferMode::Unsafe)),
      (#[buffer(copy)]) => Some(AttributeModifier::Buffer(BufferMode::Copy)),
      (#[buffer(detach)]) => Some(AttributeModifier::Buffer(BufferMode::Detach)),
      (#[global]) => Some(AttributeModifier::Global),
      (#[allow ($_rule:path)]) => None,
      (#[doc = $_attr:literal]) => None,
    })
  }).map_err(|_| AttributeError::InvalidAttribute(stringify_token(attr)))?;
  Ok(res)
}

fn parse_numeric_type(tp: &Path) -> Result<NumericArg, ArgError> {
  if tp.segments.len() == 1 {
    let segment = tp.segments.first().unwrap().ident.to_string();
    for numeric in NumericArg::iter() {
      if Into::<&'static str>::into(numeric) == segment.as_str() {
        return Ok(numeric);
      }
    }
  }

  let res = std::panic::catch_unwind(|| {
    rules!(tp.into_token_stream() => {
      ( $( std :: ffi :: )? c_void ) => NumericArg::__VOID__,
    })
  })
  .map_err(|_| ArgError::InvalidNumericType(stringify_token(tp)))?;

  Ok(res)
}

/// Parse a raw type into a container + type, allowing us to simplify the typechecks elsewhere in
/// this code.
fn parse_type_path(
  position: Position,
  attrs: Attributes,
  is_ref: bool,
  tp: &TypePath,
) -> Result<ParsedTypeContainer, ArgError> {
  use ParsedType::*;
  use ParsedTypeContainer::*;

  let buffer_mode = || match attrs.primary {
    Some(AttributeModifier::Buffer(mode)) => Ok(mode),
    _ => Err(ArgError::MissingAttribute("buffer", stringify_token(tp))),
  };

  let tokens = tp.clone().into_token_stream();
  let res = if let Ok(numeric) = parse_numeric_type(&tp.path) {
    CBare(TNumeric(numeric))
  } else {
    std::panic::catch_unwind(|| {
    rules!(tokens => {
      ( $( std :: str  :: )? String ) => {
        Ok(CBare(TString(Strings::String)))
      }
      // Note that the reference is checked below
      ( $( std :: str :: )? str ) => {
        Ok(CBare(TString(Strings::RefStr)))
      }
      ( $( std :: borrow :: )? Cow < $( $_lt:lifetime , )? str > ) => {
        Ok(CBare(TString(Strings::CowStr)))
      }
      ( $( std :: borrow :: )? Cow < $( $_lt:lifetime , )? [ u8 ] > ) => {
        Ok(CBare(TString(Strings::CowByte)))
      }
      ( $( std :: vec ::)? Vec < $ty:path > ) => {
        Ok(CBare(TBuffer(Buffer::Vec(parse_numeric_type(&ty)?))))
      }
      ( $( std :: boxed ::)? Box < [ $ty:path ] > ) => {
        Ok(CBare(TBuffer(Buffer::BoxSlice(parse_numeric_type(&ty)?))))
      }
      ( $( serde_v8 :: )? V8Slice ) => {
        Ok(CBare(TBuffer(Buffer::V8Slice(buffer_mode()?))))
      }
      ( $( serde_v8 :: )? JsBuffer ) => {
        Ok(CBare(TBuffer(Buffer::JsBuffer(buffer_mode()?))))
      }
      ( $( bytes :: )? Bytes ) => {
        Ok(CBare(TBuffer(Buffer::Bytes(buffer_mode()?))))
      }
      ( $( bytes :: )? BytesMut ) => {
        Ok(CBare(TBuffer(Buffer::BytesMut(buffer_mode()?))))
      }
      ( OpState ) => Ok(CBare(TSpecial(Special::OpState))),
      ( v8 :: HandleScope $( < $_scope:lifetime >)? ) => Ok(CBare(TSpecial(Special::HandleScope))),
      ( v8 :: FastApiCallbackOptions ) => Ok(CBare(TSpecial(Special::FastApiCallbackOptions))),
      ( v8 :: Local < $( $_scope:lifetime , )? v8 :: $v8:ident >) => Ok(CV8Local(TV8(parse_v8_type(&v8)?))),
      ( v8 :: Global < $( $_scope:lifetime , )? v8 :: $v8:ident >) => Ok(CV8Global(TV8(parse_v8_type(&v8)?))),
      ( v8 :: $v8:ident ) => Ok(CBare(TV8(parse_v8_type(&v8)?))),
      ( Rc < RefCell < $ty:ty > > ) => Ok(CRcRefCell(TSpecial(parse_type_special(position, attrs, &ty)?))),
      ( Option < $ty:ty > ) => {
        match parse_type(position, attrs, &ty)? {
          Arg::Special(special) => Ok(COption(TSpecial(special))),
          Arg::String(string) => Ok(COption(TString(string))),
          Arg::Numeric(numeric) => Ok(COption(TNumeric(numeric))),
          Arg::Buffer(buffer) => Ok(COption(TBuffer(buffer))),
          Arg::V8Ref(RefType::Ref, v8) => Ok(COption(TV8(v8))),
          Arg::V8Ref(RefType::Mut, v8) => Ok(COption(TV8Mut(v8))),
          Arg::V8Local(v8) => Ok(COptionV8Local(TV8(v8))),
          Arg::V8Global(v8) => Ok(COptionV8Global(TV8(v8))),
          _ => Err(ArgError::InvalidType(stringify_token(ty), "for option"))
        }
      }
      ( $any:ty ) => Err(ArgError::InvalidTypePath(stringify_token(any))),
    })
  }).map_err(|e| ArgError::InternalError(format!("parse_type_path {e:?}")))??
  };

  // Ensure that we have the correct reference state. This is a bit awkward but it's
  // the easiest way to work with the 'rules!' macro above.
  match res {
    // OpState appears in both ways
    CBare(TSpecial(Special::OpState)) => {}
    CBare(
      TString(Strings::RefStr) | TSpecial(Special::HandleScope) | TV8(_),
    ) => {
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

  res.validate_attributes(position, attrs, &tp)?;

  Ok(res)
}

fn parse_v8_type(v8: &Ident) -> Result<V8Arg, ArgError> {
  let v8 = v8.to_string();
  V8Arg::try_from(v8.as_str()).map_err(|_| ArgError::InvalidV8Type(v8))
}

fn parse_type_special(
  position: Position,
  attrs: Attributes,
  ty: &Type,
) -> Result<Special, ArgError> {
  match parse_type(position, attrs, ty)? {
    Arg::Special(special) => Ok(special),
    _ => Err(ArgError::InvalidType(
      stringify_token(ty),
      "for special type",
    )),
  }
}

fn parse_type_state(ty: &Type) -> Result<Arg, ArgError> {
  let s = match ty {
    Type::Path(of) => {
      let inner_type = std::panic::catch_unwind(|| {
        rules!(of.into_token_stream() => {
          (Option< $ty:ty >) => ty,
        })
      })
      .map_err(|_| ArgError::InvalidStateType(stringify_token(ty)))?;
      match parse_type_state(&inner_type)? {
        Arg::State(reftype, state) => Arg::OptionState(reftype, state),
        _ => return Err(ArgError::InvalidStateType(stringify_token(ty))),
      }
    }
    Type::Reference(of) => {
      if of.mutability.is_some() {
        Arg::State(RefType::Mut, stringify_token(&of.elem))
      } else {
        Arg::State(RefType::Ref, stringify_token(&of.elem))
      }
    }
    _ => return Err(ArgError::InvalidStateType(stringify_token(ty))),
  };
  Ok(s)
}

pub(crate) fn parse_type(
  position: Position,
  attrs: Attributes,
  ty: &Type,
) -> Result<Arg, ArgError> {
  use ParsedType::*;
  use ParsedTypeContainer::*;

  if let Some(primary) = attrs.primary {
    match primary {
      AttributeModifier::Serde => match ty {
        Type::Tuple(of) => {
          return Ok(Arg::SerdeV8(stringify_token(of)));
        }
        Type::Path(of) => {
          // If this type will parse without #[serde] (or with #[string]), it is illegal to use this type with #[serde]
          if parse_type_path(position, Attributes::default(), false, of).is_ok()
          {
            return Err(ArgError::InvalidSerdeAttributeType(stringify_token(
              ty,
            )));
          }
          // If this type will parse without #[serde] (or with #[string]), it is illegal to use this type with #[serde]
          if parse_type_path(position, Attributes::string(), false, of).is_ok()
          {
            return Err(ArgError::InvalidSerdeAttributeType(stringify_token(
              ty,
            )));
          }

          // Denylist of serde_v8 types with better alternatives
          let ty = of.into_token_stream();
          let token = stringify_token(of.path.clone());
          if let Ok(Some(err)) = std::panic::catch_unwind(|| {
            rules!(ty => {
              ( $( serde_v8:: )? Value $( < $_lifetime:lifetime >)? ) => Some("use v8::Value"),
              ( $_ty:ty ) => None,
            })
          }) {
            return Err(ArgError::InvalidSerdeType(stringify_token(ty), err));
          }

          return Ok(Arg::SerdeV8(token));
        }
        _ => {
          return Err(ArgError::InvalidSerdeAttributeType(stringify_token(ty)))
        }
      },
      AttributeModifier::State => {
        return parse_type_state(ty);
      }
      AttributeModifier::String(_)
      | AttributeModifier::Buffer(_)
      | AttributeModifier::Bigint
      | AttributeModifier::Global => {
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
        Err(ArgError::InvalidType(stringify_token(ty), "for tuple"))
      }
    }
    Type::Reference(of) => {
      let mut_type = if of.mutability.is_some() {
        RefType::Mut
      } else {
        RefType::Ref
      };
      match &*of.elem {
        // Note that we only allow numeric slices here -- if we decide to allow slices of things like v8 values,
        // this branch will need to be re-written.
        Type::Slice(of) => {
          if let Type::Path(path) = &*of.elem {
            match parse_numeric_type(&path.path)? {
              NumericArg::__VOID__ => {
                Ok(Arg::External(External::Ptr(mut_type)))
              }
              numeric => {
                let buffer = Buffer::Slice(mut_type, numeric);
                let res = CBare(TBuffer(buffer));
                res.validate_attributes(position, attrs, &of)?;
                Ok(Arg::Buffer(buffer))
              }
            }
          } else {
            Err(ArgError::InvalidType(stringify_token(ty), "for slice"))
          }
        }
        Type::Path(of) => match parse_type_path(position, attrs, true, of)? {
          CBare(TString(Strings::RefStr)) => Ok(Arg::String(Strings::RefStr)),
          COption(TString(Strings::RefStr)) => {
            Ok(Arg::OptionString(Strings::RefStr))
          }
          CBare(TV8(v8)) => Ok(Arg::V8Ref(mut_type, v8)),
          CBare(TSpecial(special)) => Ok(Arg::Ref(mut_type, special)),
          _ => Err(ArgError::InvalidType(
            stringify_token(ty),
            "for reference path",
          )),
        },
        _ => Err(ArgError::InvalidType(stringify_token(ty), "for reference")),
      }
    }
    Type::Ptr(of) => {
      let mut_type = if of.mutability.is_some() {
        RefType::Mut
      } else {
        RefType::Ref
      };
      match &*of.elem {
        Type::Path(of) => match parse_numeric_type(&of.path)? {
          NumericArg::__VOID__ => Ok(Arg::External(External::Ptr(mut_type))),
          numeric => {
            let buffer = Buffer::Ptr(mut_type, numeric);
            let res = CBare(TBuffer(buffer));
            res.validate_attributes(position, attrs, &of)?;
            Ok(Arg::Buffer(buffer))
          }
        },
        _ => Err(ArgError::InvalidType(stringify_token(ty), "for pointer")),
      }
    }
    Type::Path(of) => match parse_type_path(position, attrs, false, of)? {
      CBare(TNumeric(numeric)) => Ok(Arg::Numeric(numeric)),
      CBare(TSpecial(special)) => Ok(Arg::Special(special)),
      CBare(TString(string)) => Ok(Arg::String(string)),
      CBare(TBuffer(buffer)) => Ok(Arg::Buffer(buffer)),
      COption(TNumeric(special)) => Ok(Arg::OptionNumeric(special)),
      COption(TSpecial(special)) => Ok(Arg::Option(special)),
      COption(TString(string)) => Ok(Arg::OptionString(string)),
      CRcRefCell(TSpecial(special)) => Ok(Arg::RcRefCell(special)),
      COptionV8Local(TV8(v8)) => Ok(Arg::OptionV8Local(v8)),
      COptionV8Global(TV8(v8)) => Ok(Arg::OptionV8Global(v8)),
      COption(TV8(v8)) => Ok(Arg::OptionV8Ref(RefType::Ref, v8)),
      COption(TV8Mut(v8)) => Ok(Arg::OptionV8Ref(RefType::Mut, v8)),
      CV8Local(TV8(v8)) => Ok(Arg::V8Local(v8)),
      CV8Global(TV8(v8)) => Ok(Arg::V8Global(v8)),
      _ => Err(ArgError::InvalidType(stringify_token(ty), "for path")),
    },
    _ => Err(ArgError::InvalidType(
      stringify_token(ty),
      "for top-level type",
    )),
  }
}

fn parse_arg(arg: FnArg) -> Result<Arg, ArgError> {
  let FnArg::Typed(typed) = arg else {
    return Err(ArgError::InvalidSelf);
  };
  parse_type(Position::Arg, parse_attributes(&typed.attrs)?, &typed.ty)
}

#[cfg(test)]
mod tests {
  use super::*;
  use syn::parse_str;
  use syn::ItemFn;

  // We can't test pattern args :/
  // https://github.com/rust-lang/rfcs/issues/2688
  macro_rules! test {
    (
      // Function attributes
      $(# [ $fn_attr:meta ])?
      // fn name < 'scope, GENERIC1, GENERIC2, ... >
      $(async fn $name1:ident)?
      $(fn $name2:ident)?
      $( < $scope:lifetime $( , $generic:ident)* >)?
      (
        // Argument attribute, argument
        $( $(# [ $attr:meta ])? $ident:ident : $ty:ty ),*
      )
      // Return value
      $(-> $(# [ $ret_attr:meta ])? $ret:ty)?
      // Where clause
      $( where $($trait:ident : $bounds:ty),* )?
      ;
      // Expected return value
      $( < $( $lifetime_res:lifetime )? $(, $generic_res:ident : $bounds_res:ty )* >)? ( $( $arg_res:expr ),* ) -> $ret_res:expr ) => {
      #[test]
      fn $($name1)? $($name2)? () {
        test(
          stringify!($( #[$fn_attr] )? $(async fn $name1)? $(fn $name2)? $( < $scope $( , $generic)* >)? ( $( $( #[$attr] )? $ident : $ty ),* ) $(-> $( #[$ret_attr] )? $ret)? $( where $($trait : $bounds),* )? {}),
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
      args_expected.replace('\n', " "),
      format!("{:?}", sig.args)
        .trim_matches(|c| c == '[' || c == ']')
        .replace('\n', " ")
        .replace('"', "")
        // Use the turbofish syntax (ugly but it's just for tests)
        .replace('<', "::<")
    );
    assert_eq!(
      return_expected,
      format!("{:?}", sig.ret_val)
        .replace('"', "")
        // Use the turbofish syntax (ugly but it's just for tests)
        .replace('<', "::<")
    );
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
    fn op_slices(#[buffer] r#in: &[u8], #[buffer] out: &mut [u8]);
    (Buffer(Slice(Ref, u8)), Buffer(Slice(Mut, u8))) -> Infallible(Void)
  );
  test!(
    #[serde] fn op_serde(#[serde] input: package::SerdeInputType) -> Result<package::SerdeReturnType, Error>;
    (SerdeV8(package::SerdeInputType)) -> Result(SerdeV8(package::SerdeReturnType))
  );
  // Note the turbofish syntax here because of macro constraints
  test!(
    #[serde] fn op_serde_option(#[serde] maybe: Option<package::SerdeInputType>) -> Result<Option<package::SerdeReturnType>, Error>;
    (SerdeV8(Option::<package::SerdeInputType>)) -> Result(SerdeV8(Option::<package::SerdeReturnType>))
  );
  test!(
    #[serde] fn op_serde_tuple(#[serde] input: (A, B)) -> (A, B);
    (SerdeV8((A, B))) -> Infallible(SerdeV8((A, B)))
  );
  test!(
    fn op_local(input: v8::Local<v8::String>) -> Result<v8::Local<v8::String>, Error>;
    (V8Local(String)) -> Result(V8Local(String))
  );
  test!(
    fn op_resource(#[smi] rid: ResourceId, #[buffer] buffer: &[u8]);
    (Numeric(__SMI__), Buffer(Slice(Ref, u8))) ->  Infallible(Void)
  );
  test!(
    #[smi] fn op_resource2(#[smi] rid: ResourceId) -> Result<ResourceId, Error>;
    (Numeric(__SMI__)) -> Result(Numeric(__SMI__))
  );
  test!(
    fn op_option_numeric_result(state: &mut OpState) -> Result<Option<u32>, AnyError>;
    (Ref(Mut, OpState)) -> Result(OptionNumeric(u32))
  );
  test!(
    fn op_ffi_read_f64(state: &mut OpState, ptr: *mut c_void, #[bigint] offset: isize) -> Result <f64, AnyError>;
    (Ref(Mut, OpState), External(Ptr(Mut)), Numeric(isize)) -> Result(Numeric(f64))
  );
  test!(
    fn op_ptr_out(ptr: *const c_void) -> *mut c_void;
    (External(Ptr(Ref))) -> Infallible(External(Ptr(Mut)))
  );
  test!(
    fn op_print(#[string] msg: &str, is_err: bool) -> Result<(), Error>;
    (String(RefStr), Numeric(bool)) -> Result(Void)
  );
  test!(
    #[string] fn op_lots_of_strings(#[string] s: String, #[string] s2: Option<String>, #[string] s3: Cow<str>, #[string(onebyte)] s4: Cow<[u8]>) -> String;
    (String(String), OptionString(String), String(CowStr), String(CowByte)) -> Infallible(String(String))
  );
  test!(
    #[string] fn op_lots_of_option_strings(#[string] s: Option<String>, #[string] s2: Option<&str>, #[string] s3: Option<Cow<str>>) -> Option<String>;
    (OptionString(String), OptionString(RefStr), OptionString(CowStr)) -> Infallible(OptionString(String))
  );
  test!(
    fn op_scope<'s>(#[string] msg: &'s str);
    <'s> (String(RefStr)) -> Infallible(Void)
  );
  test!(
    fn op_scope_and_generics<'s, AB, BC>(#[string] msg: &'s str) where AB: some::Trait, BC: OtherTrait;
    <'s, AB: some::Trait, BC: OtherTrait> (String(RefStr)) -> Infallible(Void)
  );
  test!(
    fn op_generics_static<'s, AB, BC>(#[string] msg: &'s str) where AB: some::Trait + 'static, BC: OtherTrait;
    <'s, AB: some::Trait + 'static, BC: OtherTrait> (String(RefStr)) -> Infallible(Void)
  );
  test!(
    fn op_v8_types(s: &mut v8::String, sopt: Option<&mut v8::String>, s2: v8::Local<v8::String>, #[global] s3: v8::Global<v8::String>);
    (V8Ref(Mut, String), OptionV8Ref(Mut, String), V8Local(String), V8Global(String)) -> Infallible(Void)
  );
  test!(
    fn op_v8_scope<'s>(scope: &mut v8::HandleScope<'s>);
    <'s> (Ref(Mut, HandleScope)) -> Infallible(Void)
  );
  test!(
    fn op_state_rc(state: Rc<RefCell<OpState>>);
    (RcRefCell(OpState)) -> Infallible(Void)
  );
  test!(
    fn op_state_ref(state: &OpState);
    (Ref(Ref, OpState)) -> Infallible(Void)
  );
  test!(
    fn op_state_attr(#[state] something: &Something, #[state] another: Option<&Something>);
    (State(Ref, Something), OptionState(Ref, Something)) -> Infallible(Void)
  );
  test!(
    #[buffer] fn op_buffers(#[buffer(copy)] a: Vec<u8>, #[buffer(copy)] b: Box<[u8]>, #[buffer(copy)] c: bytes::Bytes, #[buffer] d: V8Slice, #[buffer] e: JsBuffer, #[buffer(detach)] f: JsBuffer) -> Vec<u8>;
    (Buffer(Vec(u8)), Buffer(BoxSlice(u8)), Buffer(Bytes(Copy)), Buffer(V8Slice(Default)), Buffer(JsBuffer(Default)), Buffer(JsBuffer(Detach))) -> Infallible(Buffer(Vec(u8)))
  );
  test!(
    #[buffer] fn op_return_bytesmut() -> bytes::BytesMut;
    () -> Infallible(Buffer(BytesMut(Default)))
  );
  test!(
    async fn op_async_void();
    () -> Future(Void)
  );
  test!(
    async fn op_async_result_void() -> Result<()>;
    () -> FutureResult(Void)
  );
  test!(
    fn op_async_impl_void() -> impl Future<Output = ()>;
    () -> Future(Void)
  );
  test!(
    fn op_async_result_impl_void() -> Result<impl Future<Output = ()>, Error>;
    () -> ResultFuture(Void)
  );
  // Args

  expect_fail!(
    op_with_bad_string1,
    ArgError("s", MissingAttribute("string", "str")),
    fn f(s: &str) {}
  );
  expect_fail!(
    op_with_bad_string2,
    ArgError("s", MissingAttribute("string", "String")),
    fn f(s: String) {}
  );
  expect_fail!(
    op_with_bad_string3,
    ArgError("s", MissingAttribute("string", "Cow<str>")),
    fn f(s: Cow<str>) {}
  );
  expect_fail!(
    op_with_invalid_string,
    ArgError("x", InvalidAttributeType("string", "u32")),
    fn f(#[string] x: u32) {}
  );
  expect_fail!(
    op_with_invalid_buffer,
    ArgError("x", InvalidAttributeType("buffer", "u32")),
    fn f(#[buffer] x: u32) {}
  );
  expect_fail!(
    op_with_bad_attr,
    RetError(AttributeError(InvalidAttribute("#[badattr]"))),
    #[badattr]
    fn f() {}
  );
  expect_fail!(
    op_with_bad_attr2,
    ArgError("a", AttributeError(InvalidAttribute("#[badattr]"))),
    fn f(#[badattr] a: u32) {}
  );
  expect_fail!(
    op_with_missing_global,
    ArgError("g", MissingAttribute("global", "v8::Global<v8::String>")),
    fn f(g: v8::Global<v8::String>) {}
  );
  expect_fail!(
    op_with_invalid_global,
    ArgError("l", InvalidAttributeType("global", "v8::Local<v8::String>")),
    fn f(#[global] l: v8::Local<v8::String>) {}
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

  expect_fail!(
    op_with_bad_serde_string,
    ArgError("s", InvalidSerdeAttributeType("String")),
    fn f(#[serde] s: String) {}
  );
  expect_fail!(
    op_with_bad_serde_str,
    ArgError("s", InvalidSerdeAttributeType("&str")),
    fn f(#[serde] s: &str) {}
  );
  expect_fail!(
    op_with_bad_serde_value,
    ArgError("v", InvalidSerdeType("serde_v8::Value", "use v8::Value")),
    fn f(#[serde] v: serde_v8::Value) {}
  );
}
