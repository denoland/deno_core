// Copyright 2018-2025 the Deno authors. MIT license.

use proc_macro2::TokenStream;
use proc_macro2::{Ident, Span};
use quote::ToTokens;
use quote::TokenStreamExt;
use quote::format_ident;
use quote::quote;
use std::collections::BTreeMap;
use syn::parse::Parse;
use syn::parse::ParseStream;

use strum::{IntoEnumIterator, IntoStaticStr};
use strum_macros::EnumIter;
use strum_macros::EnumString;
use syn::Attribute;
use syn::FnArg;
use syn::GenericParam;
use syn::Generics;
use syn::Meta;
use syn::Pat;
use syn::Path;
use syn::Signature;
use syn::Token;
use syn::Type;
use syn::TypeParamBound;
use syn::TypePath;
use syn::WherePredicate;
use syn::punctuated::Punctuated;
use syn::{AttrStyle, GenericArgument, PathArguments};
use thiserror::Error;

use super::signature_retval::RetVal;

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

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Special {
  HandleScope,
  OpState,
  JsRuntimeState,
  FastApiCallbackOptions,
  Isolate,
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
pub enum BufferType {
  /// Shared/unowned, may be resizable. [`&[u8]`], [`&mut [u8]`], [`&[u32]`], etc...
  Slice(RefType, NumericArg),
  /// Shared/unowned, may be resizable. [`*const u8`], [`*mut u8`], [`*const u32`], etc...
  Ptr(RefType, NumericArg),
  /// Owned, copy. [`Box<[u8]>`], [`Box<[u32]>`], etc...
  BoxSlice(NumericArg),
  /// Owned, copy. [`Vec<u8>`], [`Vec<u32>`], etc...
  Vec(NumericArg),
  /// Maybe shared or a copy. Stored in `bytes::Bytes`
  Bytes,
  /// Owned, copy. Stored in `bytes::BytesMut`
  BytesMut,
  /// Shared, not resizable (or resizable and detatched), stored in `serde_v8::V8Slice`
  V8Slice(NumericArg),
  /// Shared, not resizable (or resizable and detatched), stored in `serde_v8::JsBuffer`
  JsBuffer,
}

impl BufferType {
  pub const fn valid_modes(
    &self,
    position: Position,
  ) -> &'static [AttributeModifier] {
    use BufferType::*;
    // For each mode, apply it to TypedArray, ArrayBuffer, and Any.
    macro_rules! expand {
      ($($mode:ident),*) => {
        &[$(
          AttributeModifier::Buffer(BufferMode::$mode, BufferSource::TypedArray),
          AttributeModifier::Buffer(BufferMode::$mode, BufferSource::ArrayBuffer),
          AttributeModifier::Buffer(BufferMode::$mode, BufferSource::Any),
        )*]
      };
      (extra = $t:expr_2021, $($mode:ident),*) => {
        &[$t, $(
          AttributeModifier::Buffer(BufferMode::$mode, BufferSource::TypedArray),
          AttributeModifier::Buffer(BufferMode::$mode, BufferSource::ArrayBuffer),
          AttributeModifier::Buffer(BufferMode::$mode, BufferSource::Any),
        )*]
      };
    }
    match position {
      Position::Arg => match self {
        Bytes | BytesMut | Vec(..) | BoxSlice(..) => {
          expand!(Copy)
        }
        JsBuffer | V8Slice(..) => expand!(Copy, Detach, Default),
        Slice(..) | Ptr(..) => expand!(Default),
      },
      Position::RetVal => match self {
        Bytes | BytesMut | JsBuffer | V8Slice(..) | Vec(..) | BoxSlice(..) => {
          expand!(Default)
        }
        Slice(..) | Ptr(..) => expand!(Default),
      },
    }
  }

  pub const fn element(&self) -> NumericArg {
    match self {
      Self::Slice(_, arg) => *arg,
      Self::BoxSlice(arg) => *arg,
      Self::Bytes | Self::BytesMut | Self::JsBuffer => NumericArg::u8,
      Self::Ptr(_, arg) => *arg,
      Self::Vec(arg) => *arg,
      Self::V8Slice(arg) => *arg,
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

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum NumericFlag {
  None,
  Number,
}

// its own struct to facility Eq & PartialEq on other structs
#[derive(Clone, Debug)]
pub struct WebIDLPair(pub Ident, pub syn::Expr);
impl PartialEq for WebIDLPair {
  fn eq(&self, other: &Self) -> bool {
    self.0 == other.0
  }
}
impl Eq for WebIDLPair {}

impl Parse for WebIDLPair {
  fn parse(input: ParseStream) -> syn::Result<Self> {
    let key: Ident = input.parse()?;
    input.parse::<syn::token::Eq>()?;
    Ok(WebIDLPair(key, input.parse()?))
  }
}

#[derive(Clone, Debug)]
pub struct WebIDLDefault(pub syn::Expr);
impl PartialEq for WebIDLDefault {
  fn eq(&self, _other: &Self) -> bool {
    true
  }
}
impl Eq for WebIDLDefault {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WebIDLArgs {
  pub default: Option<WebIDLDefault>,
  pub options: Vec<WebIDLPair>,
}

impl Parse for WebIDLArgs {
  fn parse(input: ParseStream) -> syn::Result<Self> {
    let mut default: Option<WebIDLDefault> = None;
    let mut options: Vec<WebIDLPair> = Vec::new();

    while !input.is_empty() {
      let key: Ident = input.parse()?;

      if key == "default" {
        if default.is_some() {
          return Err(syn::Error::new(
            key.span(),
            "duplicate `default` argument",
          ));
        }
        input.parse::<Token![=]>()?;
        default = Some(WebIDLDefault(input.parse::<syn::Expr>()?));
      } else if key == "options" {
        if !options.is_empty() {
          return Err(syn::Error::new(
            key.span(),
            "duplicate `options` argument",
          ));
        }
        let content;
        syn::parenthesized!(content in input);
        let parsed_options =
          content.parse_terminated(WebIDLPair::parse, Token![,])?;
        options = parsed_options.into_iter().collect();
      } else {
        return Err(syn::Error::new(
          key.span(),
          "unknown webidl argument, expected `default` or `options`",
        ));
      }

      if !input.is_empty() {
        input.parse::<Token![,]>()?;
      }
    }

    Ok(WebIDLArgs { default, options })
  }
}

/// Args are not a 1:1 mapping with Rust types, rather they represent broad classes of types that
/// tend to have similar argument handling characteristics. This may need one more level of indirection
/// given how many of these types have option variants, however.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Arg {
  Void,
  VoidUndefined,
  Special(Special),
  String(Strings),
  Buffer(BufferType, BufferMode, BufferSource),
  External(External),
  Ref(RefType, Special),
  Rc(Special),
  RcRefCell(Special),
  Option(Special),
  OptionString(Strings),
  OptionNumeric(NumericArg, NumericFlag),
  OptionBuffer(BufferType, BufferMode, BufferSource),
  OptionV8Local(V8Arg),
  V8Local(V8Arg),
  OptionV8Ref(RefType, V8Arg),
  V8Ref(RefType, V8Arg),
  Numeric(NumericArg, NumericFlag),
  SerdeV8(String),
  CppGcResource(bool, String),
  OptionCppGcResource(String),
  CppGcProtochain(Vec<String>),
  FromV8(String, bool),
  ToV8(String),
  WebIDL(String, Vec<WebIDLPair>, Option<WebIDLDefault>),
  VarArgs,
  This,
}

impl Arg {
  fn from_parsed(
    parsed: ParsedTypeContainer,
    position: Position,
    attr: Attributes,
  ) -> Result<Self, ArgError> {
    use ParsedType::*;
    use ParsedTypeContainer::*;

    let buffer_mode = || match attr.primary {
      Some(AttributeModifier::Buffer(mode, _)) => Ok(mode),
      _ => Err(ArgError::MissingAttribute("buffer", format!("{parsed:?}"))),
    };

    let buffer_source = || match attr.primary {
      Some(AttributeModifier::Buffer(_, source)) => Ok(source),
      _ => Err(ArgError::MissingAttribute("buffer", format!("{parsed:?}"))),
    };

    match parsed {
      CBare(TNumeric(numeric)) => Ok(Arg::Numeric(numeric, NumericFlag::None)),
      CBare(TSpecial(special)) => Ok(Arg::Special(special)),
      CBare(TString(string)) => Ok(Arg::String(string)),
      CBare(TBuffer(buffer)) => {
        Ok(Arg::Buffer(buffer, buffer_mode()?, buffer_source()?))
      }
      COption(TNumeric(special)) => {
        Ok(Arg::OptionNumeric(special, NumericFlag::None))
      }
      COption(TSpecial(special)) => Ok(Arg::Option(special)),
      COption(TString(string)) => Ok(Arg::OptionString(string)),
      COption(TBuffer(buffer)) => {
        Ok(Arg::OptionBuffer(buffer, buffer_mode()?, buffer_source()?))
      }
      CRc(TSpecial(special)) => Ok(Arg::Rc(special)),
      CRcRefCell(TSpecial(special)) => Ok(Arg::RcRefCell(special)),
      COptionV8Local(TV8(v8)) => Ok(Arg::OptionV8Local(v8)),
      COption(TV8(v8)) => Ok(Arg::OptionV8Ref(RefType::Ref, v8)),
      COption(TV8Mut(v8)) => Ok(Arg::OptionV8Ref(RefType::Mut, v8)),
      CV8Local(TV8(v8)) => Ok(Arg::V8Local(v8)),
      CUnknown(t, slow) => match position {
        Position::Arg => Ok(Arg::FromV8(stringify_token(t), slow)),
        Position::RetVal => Ok(Arg::ToV8(stringify_token(t))),
      },
      _ => unreachable!(),
    }
  }

  /// Is this argument virtual? ie: does it come from the æther rather than a concrete JavaScript input
  /// argument?
  #[allow(clippy::match_like_matches_macro)]
  pub const fn is_virtual(&self) -> bool {
    match self {
      Self::Special(
        Special::FastApiCallbackOptions
        | Special::OpState
        | Special::JsRuntimeState
        | Special::HandleScope
        | Special::Isolate,
      ) => true,
      Self::Ref(
        _,
        Special::FastApiCallbackOptions
        | Special::OpState
        | Special::JsRuntimeState
        | Special::HandleScope
        | Special::Isolate,
      ) => true,
      Self::RcRefCell(
        Special::FastApiCallbackOptions
        | Special::OpState
        | Special::JsRuntimeState
        | Special::HandleScope,
      ) => true,
      Self::This | Self::VarArgs => true,
      _ => false,
    }
  }

  /// Convert the [`Arg`] into a [`TokenStream`] representing the fully-qualified type.
  #[allow(unused)] // unused for now but keeping
  pub fn type_token(&self, deno_core: &TokenStream) -> TokenStream {
    match self {
      Arg::V8Ref(RefType::Ref, v8) => quote!(&deno_core::v8::#v8),
      Arg::V8Ref(RefType::Mut, v8) => quote!(&mut deno_core::v8::#v8),
      Arg::V8Local(v8) => quote!(deno_core::v8::Local<deno_core::v8::#v8>),
      Arg::OptionV8Ref(RefType::Ref, v8) => {
        quote!(::std::option::Option<&deno_core::v8::#v8>)
      }
      Arg::OptionV8Ref(RefType::Mut, v8) => {
        quote!(::std::option::Option<&mut deno_core::v8::#v8>)
      }
      Arg::OptionV8Local(v8) => {
        quote!(::std::option::Option<deno_core::v8::Local<deno_core::v8::#v8>>)
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
        | Arg::OptionNumeric(..)
        | Arg::Option(..)
        | Arg::OptionString(..)
        | Arg::OptionBuffer(..)
        | Arg::OptionCppGcResource(..)
    )
  }

  /// Return the `Some` part of this `Option` type, or `None` if it is not an `Option`.
  pub fn some_type(&self) -> Option<Arg> {
    Some(match self {
      Arg::OptionV8Ref(r, t) => Arg::V8Ref(*r, *t),
      Arg::OptionV8Local(t) => Arg::V8Local(*t),
      Arg::OptionNumeric(t, flag) => Arg::Numeric(*t, *flag),
      Arg::Option(t) => Arg::Special(t.clone()),
      Arg::OptionString(t) => Arg::String(*t),
      Arg::OptionBuffer(t, m, s) => Arg::Buffer(*t, *m, *s),
      Arg::OptionCppGcResource(t) => Arg::CppGcResource(false, t.clone()),
      _ => return None,
    })
  }

  /// This must be kept in sync with the `RustToV8`/`RustToV8Fallible` implementations in `deno_core`. If
  /// this falls out of sync, you will see compile errors.
  pub fn slow_retval(&self) -> ArgSlowRetval {
    match self.some_type() {
      Some(some) => {
        // If this is an optional return value, we use the same return type as the underlying object.
        match some.slow_retval() {
          // We need a scope in the case of an option so we can allocate a null
          ArgSlowRetval::V8LocalNoScope => ArgSlowRetval::RetVal,
          rv => rv,
        }
      }
      _ => {
        match self {
          Arg::Numeric(
            NumericArg::i64
            | NumericArg::u64
            | NumericArg::isize
            | NumericArg::usize,
            NumericFlag::None,
          ) => ArgSlowRetval::V8Local,
          Arg::Numeric(
            NumericArg::i64
            | NumericArg::u64
            | NumericArg::isize
            | NumericArg::usize,
            NumericFlag::Number,
          ) => ArgSlowRetval::RetVal,
          Arg::VoidUndefined => ArgSlowRetval::V8LocalNoScope,
          Arg::Void | Arg::Numeric(..) => ArgSlowRetval::RetVal,
          Arg::External(_) => ArgSlowRetval::V8Local,
          // Fast return value path for empty strings
          Arg::String(_) => ArgSlowRetval::RetValFallible,
          Arg::SerdeV8(_) => ArgSlowRetval::V8LocalFalliable,
          Arg::ToV8(_) => ArgSlowRetval::V8LocalFalliable,
          // No scope required for these
          Arg::V8Local(_) => ArgSlowRetval::V8LocalNoScope,
          // ArrayBuffer is infallible
          Arg::Buffer(.., BufferSource::ArrayBuffer) => ArgSlowRetval::V8Local,
          // TypedArray is fallible
          Arg::Buffer(.., BufferSource::TypedArray) => {
            ArgSlowRetval::V8LocalFalliable
          }
          // ArrayBuffer is infallible
          Arg::OptionBuffer(.., BufferSource::ArrayBuffer) => {
            ArgSlowRetval::V8Local
          }
          // TypedArray is fallible
          Arg::OptionBuffer(.., BufferSource::TypedArray) => {
            ArgSlowRetval::V8LocalFalliable
          }
          Arg::CppGcResource(..) | Arg::CppGcProtochain(_) => {
            ArgSlowRetval::V8Local
          }
          _ => ArgSlowRetval::None,
        }
      }
    }
  }

  /// Does this type have a marker (used for specialization of serialization/deserialization)?
  pub fn marker(&self) -> ArgMarker {
    match self {
      Arg::Buffer(.., BufferSource::ArrayBuffer)
      | Arg::OptionBuffer(.., BufferSource::ArrayBuffer) => {
        ArgMarker::ArrayBuffer
      }
      Arg::SerdeV8(_) => ArgMarker::Serde,
      Arg::Numeric(NumericArg::__SMI__, _) => ArgMarker::Smi,
      Arg::Numeric(_, NumericFlag::Number) => ArgMarker::Number,
      Arg::CppGcProtochain(_)
      | Arg::CppGcResource(..)
      | Arg::OptionCppGcResource(_) => ArgMarker::Cppgc,
      Arg::ToV8(_) => ArgMarker::ToV8,
      Arg::VoidUndefined => ArgMarker::Undefined,
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
  /// This type should be serialized as a number.
  Number,
  /// This buffer type should be serialized as an ArrayBuffer.
  ArrayBuffer,
  /// This type should be wrapped as a cppgc V8 object.
  Cppgc,
  /// This type should be converted with `ToV8`
  ToV8,
  /// This unit type should be a undefined.
  Undefined,
}

#[derive(Debug)]
pub enum ParsedType {
  TSpecial(Special),
  TString(Strings),
  TBuffer(BufferType),
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
      ) => Some(&[AttributeModifier::Bigint, AttributeModifier::Number]),
      TBuffer(buffer) => Some(buffer.valid_modes(position)),
      TString(Strings::CowByte) => {
        Some(&[AttributeModifier::String(StringMode::OneByte)])
      }
      TString(..) => Some(&[AttributeModifier::String(StringMode::Default)]),
      _ => None,
    }
  }
}

#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub enum ParsedTypeContainer {
  CBare(ParsedType),
  COption(ParsedType),
  CRc(ParsedType),
  CRcRefCell(ParsedType),
  COptionV8Local(ParsedType),
  CV8Local(ParsedType),
  CUnknown(Type, bool),
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
      CV8Local(_) | COptionV8Local(_) | CUnknown(_, false) => None,
      CUnknown(_, true) => Some(&[AttributeModifier::V8Slow]),
      CBare(t) | COption(t) | CRcRefCell(t) | CRc(t) => {
        t.required_attributes(position)
      }
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
          ));
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
            ));
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
pub struct ParsedSignature {
  // The parsed arguments
  pub args: Vec<(Arg, Attributes)>,
  // The parsed return value
  pub ret_val: RetVal,
  // Lifetimes
  pub lifetimes: Vec<Ident>,
  // Generic bounds: each generic must have one and only simple trait bound
  pub generic_bounds: BTreeMap<Ident, String>,
  // Metadata keys and values
  pub metadata: BTreeMap<Ident, syn::Lit>,
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
pub enum BufferSource {
  /// The buffer expects an exactly-typed TypedArray of the given underlying format.
  TypedArray,
  /// The buffer expects a raw ArrayBuffer, which is an unsliced underlying backing store.
  ArrayBuffer,
  /// The buffer expects a byte-like slice which may be an ArrayBuffer, a TypedArray, or
  /// a DataView.
  Any,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AttributeModifier {
  /// #[serde], for serde_v8 types.
  Serde,
  /// #[webidl], for types that impl `WebIdlConverter`
  WebIDL(WebIDLArgs),
  /// #[smi], for non-integral ID types representing small integers (-2³¹ and 2³¹-1 on 64-bit platforms,
  /// see https://medium.com/fhinkel/v8-internals-how-small-is-a-small-integer-e0badc18b6da).
  Smi,
  /// #[string], for strings.
  String(StringMode),
  /// #[buffer], for buffers.
  Buffer(BufferMode, BufferSource),
  /// #[bigint], for u64/usize/i64/isize indicating value is a BigInt
  Bigint,
  /// #[number], for u64/usize/i64/isize indicating value is a Number
  Number,
  /// #[cppgc], for a resource backed managed by cppgc.
  CppGcResource,
  /// #[proto]
  CppGcProto,
  /// Any attribute that we may want to omit if not syntactically valid.
  Ignore,
  /// Varaible-length arguments.
  VarArgs,
  /// The `this` receiver.
  This,
  /// `undefined`
  Undefined,
  /// Use non-fast versions of FromV8/ToV8 traits
  V8Slow,
  /// Custom validator.
  Validate(Path),
}

impl AttributeModifier {
  fn name(&self) -> &'static str {
    match self {
      AttributeModifier::Bigint => "bigint",
      AttributeModifier::Number => "number",
      AttributeModifier::Buffer(..) => "buffer",
      AttributeModifier::Smi => "smi",
      AttributeModifier::Serde => "serde",
      AttributeModifier::WebIDL(_) => "webidl",
      AttributeModifier::String(_) => "string",
      AttributeModifier::CppGcResource => "cppgc",
      AttributeModifier::CppGcProto => "proto",
      AttributeModifier::Ignore => "ignore",
      AttributeModifier::VarArgs => "varargs",
      AttributeModifier::This => "this",
      AttributeModifier::Undefined => "undefined",
      AttributeModifier::V8Slow => "v8_slow",
      AttributeModifier::Validate(_) => "validate",
    }
  }
}

#[derive(Error, Debug)]
pub enum SignatureError {
  #[error("Invalid argument: '{0}'")]
  ArgError(String, #[source] ArgError),
  #[error("Invalid return type")]
  RetError(#[from] RetError),
  #[error(
    "Generic '{0}' must have one and only bound (either <T> and 'where T: Trait', or <T: Trait>)"
  )]
  GenericBoundCardinality(String),
  #[error(
    "Where clause predicate '{0}' (eg: where T: Trait) must appear in generics list (eg: <T>)"
  )]
  WherePredicateMustAppearInGenerics(String),
  #[error(
    "All generics must appear only once in the generics parameter list or where clause"
  )]
  DuplicateGeneric(String),
  #[error("Generic lifetime '{0}' may not have bounds (eg: <'a: 'b>)")]
  LifetimesMayNotHaveBounds(String),
  #[error(
    "Invalid predicate: '{0}' Only simple where predicates are allowed (eg: T: Trait)"
  )]
  InvalidWherePredicate(String),
  #[error("JsRuntimeState may only be used in one parameter")]
  InvalidMultipleJsRuntimeState,
  #[error("Invalid metadata attribute: {0}")]
  InvalidMetaAttribute(#[source] syn::Error),
}

#[derive(Error, Debug)]
pub enum AttributeError {
  #[error("Unknown or invalid attribute '{0}'")]
  InvalidAttribute(String),
  #[error(
    "Invalid inner attribute (#![attr]) in this position. Use an equivalent outer attribute (#[attr]) on the function instead."
  )]
  InvalidInnerAttribute,
}

#[derive(Error, Debug)]
pub enum ArgError {
  #[error("Invalid argument type: {0} ({1})")]
  InvalidType(String, &'static str),
  #[error("Invalid numeric argument type: {0}")]
  InvalidNumericType(String),
  #[error("Invalid numeric #[smi] argument type: {0}")]
  InvalidSmiType(String),
  #[error("The type {0} cannot be a reference")]
  InvalidReference(String),
  #[error("The type {0} must be a reference")]
  MissingReference(String),
  #[error("Invalid or deprecated #[serde] type '{0}': {1}")]
  InvalidSerdeType(String, &'static str),
  #[error("Invalid #[{0}] for type: {1}")]
  InvalidAttributeType(&'static str, String),
  #[error("Cannot use #[number] for type: {0}")]
  InvalidNumberAttributeType(String),
  #[error("Invalid v8 type: {0}")]
  InvalidV8Type(String),
  #[error("Missing a #[{0}] attribute for type: {1}")]
  MissingAttribute(&'static str, String),
  #[error("Argument attribute error")]
  AttributeError(#[from] AttributeError),
  #[error("The type '{0}' is not allowed in this position")]
  NotAllowedInThisPosition(String),
  #[error(
    "Invalid deno_core:: prefix for type '{0}'. Try adding `use deno_core::{1}` at the top of the file and specifying `{2}` in this position."
  )]
  InvalidDenoCorePrefix(String, String, String),
  #[error("Expected a reference. Use '#[cppgc] &{0}' instead.")]
  ExpectedCppGcReference(String),
  #[error("Invalid #[cppgc] type '{0}'")]
  InvalidCppGcType(String),
  #[error("#[{0}] is only valid in {1} position")]
  InvalidAttributePosition(&'static str, &'static str),
}

#[derive(Error, Debug)]
pub enum RetError {
  #[error("Invalid return type")]
  InvalidType(#[from] ArgError),
  #[error("Return value attribute error")]
  AttributeError(#[from] AttributeError),
}

#[derive(Clone, Default, Debug, Eq, PartialEq)]
pub(crate) struct Attributes {
  primary: Option<AttributeModifier>,
  pub(crate) rest: Vec<AttributeModifier>,
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
      rest: vec![],
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

struct MetadataPair {
  key: Ident,
  _eq: Token![=],
  value: syn::Lit,
}

impl Parse for MetadataPair {
  fn parse(input: ParseStream) -> syn::Result<Self> {
    Ok(Self {
      key: input.parse()?,
      _eq: input.parse()?,
      value: input.parse()?,
    })
  }
}

impl Parse for MetadataPairs {
  fn parse(input: ParseStream) -> syn::Result<Self> {
    let pairs = input.parse_terminated(MetadataPair::parse, Token![,])?;
    Ok(Self { pairs })
  }
}

struct MetadataPairs {
  pairs: syn::punctuated::Punctuated<MetadataPair, Token![,]>,
}

fn parse_metadata_pairs(
  attr: &Attribute,
) -> Result<Vec<(Ident, syn::Lit)>, SignatureError> {
  let syn::Meta::List(meta) = &attr.meta else {
    return Ok(vec![]);
  };
  if !meta.path.is_ident("meta") {
    return Ok(vec![]);
  }

  let pairs = meta
    .parse_args_with(MetadataPairs::parse)
    .map_err(SignatureError::InvalidMetaAttribute)?;
  Ok(
    pairs
      .pairs
      .into_iter()
      .map(|pair| (pair.key, pair.value))
      .collect(),
  )
}

fn parse_metadata(
  attributes: &[Attribute],
) -> Result<BTreeMap<Ident, syn::Lit>, SignatureError> {
  let mut metadata = BTreeMap::new();
  for attr in attributes {
    let pairs = parse_metadata_pairs(attr)?;
    metadata.extend(pairs);
  }
  Ok(metadata)
}

pub fn parse_signature(
  attributes: Vec<Attribute>,
  signature: Signature,
) -> Result<ParsedSignature, SignatureError> {
  let mut args = vec![];
  for input in signature.inputs {
    match &input {
      FnArg::Receiver(_) => continue,
      FnArg::Typed(arg) => {
        let name = match &*arg.pat {
          Pat::Ident(ident) => ident.ident.to_string(),
          _ => "(complex)".to_owned(),
        };

        let attrs = parse_attributes(&arg.attrs)
          .map_err(|err| SignatureError::ArgError(name.clone(), err.into()))?;
        let ty = parse_type(Position::Arg, attrs.clone(), &arg.ty)
          .map_err(|err| SignatureError::ArgError(name, err))?;

        args.push((ty, attrs));
      }
    }
  }

  let ret_val = RetVal::try_parse(
    signature.asyncness.is_some(),
    parse_attributes(&attributes).map_err(RetError::AttributeError)?,
    &signature.output,
  )?;

  let lifetimes = parse_lifetimes(&signature.generics)?;
  let generic_bounds = parse_generics(&signature.generics)?;

  let mut jsruntimestate_count = 0;

  for (arg, _) in &args {
    match arg {
      Arg::Ref(_, Special::JsRuntimeState) => {
        jsruntimestate_count += 1;
      }
      Arg::RcRefCell(Special::JsRuntimeState) => {
        jsruntimestate_count += 1;
      }
      _ => {}
    }
  }

  // Ensure that there is at most one JsRuntimeState
  if jsruntimestate_count > 1 {
    return Err(SignatureError::InvalidMultipleJsRuntimeState);
  }

  let metadata = parse_metadata(&attributes)?;

  Ok(ParsedSignature {
    args,
    ret_val,
    lifetimes,
    generic_bounds,
    metadata,
  })
}

/// Extract one lifetime from the [`syn::Generics`], ensuring that the lifetime is valid
/// and has no bounds.
fn parse_lifetimes(generics: &Generics) -> Result<Vec<Ident>, SignatureError> {
  let mut res = Vec::new();
  for param in &generics.params {
    if let GenericParam::Lifetime(lt) = param {
      if !lt.bounds.is_empty() {
        return Err(SignatureError::LifetimesMayNotHaveBounds(
          lt.lifetime.to_string(),
        ));
      }
      res.push(lt.lifetime.ident.clone());
    }
  }
  Ok(res)
}

/// Parse a bound as a string. Valid bounds include "Trait" and "Trait + 'static". All
/// other bounds are invalid.
fn parse_bound(
  bounds: &Punctuated<TypeParamBound, Token![+]>,
) -> Result<String, SignatureError> {
  let error = || {
    Err(SignatureError::InvalidWherePredicate(stringify_token(
      bounds,
    )))
  };

  let mut has_static_lifetime = false;
  let mut bound = None;
  for b in bounds {
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

  Ok(if has_static_lifetime {
    format!("{bound} + 'static")
  } else {
    bound
  })
}

/// Parse and validate generics. We require one and only one trait bound for each generic
/// parameter. Tries to sanity check and return reasonable errors for possible signature errors.
fn parse_generics(
  generics: &Generics,
) -> Result<BTreeMap<Ident, String>, SignatureError> {
  let mut where_clauses = std::collections::HashMap::<Ident, String>::new();

  // First, extract the where clause so we can detect duplicated predicates
  if let Some(where_clause) = &generics.where_clause {
    for predicate in &where_clause.predicates {
      if let WherePredicate::Type(ty) = predicate
        && !ty.bounds.is_empty()
        && let Type::Path(path) = &ty.bounded_ty
        && let Some(ident) = path.path.get_ident()
      {
        let bound = parse_bound(&ty.bounds)?;

        if where_clauses.insert(ident.clone(), bound).is_some() {
          return Err(SignatureError::DuplicateGeneric(ident.to_string()));
        }
      } else {
        return Err(SignatureError::InvalidWherePredicate(
          predicate.to_token_stream().to_string(),
        ));
      }
    }
  }

  let mut res = BTreeMap::new();
  for param in &generics.params {
    if let GenericParam::Type(ty) = param {
      let name = &ty.ident;

      let bound = if !ty.bounds.is_empty() {
        if where_clauses.contains_key(name) {
          return Err(SignatureError::GenericBoundCardinality(
            name.to_string(),
          ));
        }
        parse_bound(&ty.bounds)?
      } else {
        let Some(bound) = where_clauses.remove(name) else {
          return Err(SignatureError::GenericBoundCardinality(
            name.to_string(),
          ));
        };
        bound
      };

      if res.contains_key(name) {
        return Err(SignatureError::DuplicateGeneric(name.to_string()));
      }
      res.insert(name.clone(), bound);
    }
  }
  if !where_clauses.is_empty() {
    return Err(SignatureError::WherePredicateMustAppearInGenerics(
      where_clauses.into_keys().next().unwrap().to_string(),
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
      if attr == AttributeModifier::Ignore {
        continue;
      }
      attrs.push(attr)
    }
  }

  if attrs.len() == 1 && matches!(attrs[0], AttributeModifier::Validate(_)) {
    return Ok(Attributes {
      primary: None,
      rest: attrs,
    });
  }

  if attrs.is_empty() {
    return Ok(Attributes::default());
  }
  Ok(Attributes {
    primary: Some((*attrs.last().unwrap()).clone()),
    rest: attrs[..attrs.len() - 1].to_vec(),
  })
}

/// Is this a special attribute that we understand?
pub fn is_attribute_special(attr: &Attribute) -> bool {
  parse_attribute(attr)
      .unwrap_or_default()
      .and_then(|attr| match attr {
        AttributeModifier::Ignore => None,
        AttributeModifier::Validate(_) => None,
        _ => Some(()),
      })
      .is_some()
    // this is kind of ugly, but #[meta(..)] is the only
    // attribute that we want to omit from the generated code
    // that doesn't have a semantic meaning
    || attr.path().is_ident("meta")
}

/// Parses an attribute, returning None if this is an attribute we support but is
/// otherwise unknown (ie: doc comments).
fn parse_attribute(
  attr: &Attribute,
) -> Result<Option<AttributeModifier>, AttributeError> {
  if matches!(attr.style, AttrStyle::Inner(_)) {
    return Err(AttributeError::InvalidInnerAttribute);
  }

  let Some(ident) = attr.path().get_ident() else {
    return Ok(None);
  };

  let modifier = match ident.to_string().as_str() {
    "bigint" => Some(AttributeModifier::Bigint),
    "number" => Some(AttributeModifier::Number),
    "undefined" => Some(AttributeModifier::Undefined),
    "serde" => Some(AttributeModifier::Serde),
    "smi" => Some(AttributeModifier::Smi),
    "this" => Some(AttributeModifier::This),
    "cppgc" => Some(AttributeModifier::CppGcResource),
    "proto" => Some(AttributeModifier::CppGcProto),
    "varargs" => Some(AttributeModifier::VarArgs),
    "v8_slow" => Some(AttributeModifier::V8Slow),

    "validate" => {
      let value: Path = attr
        .parse_args()
        .map_err(|_| AttributeError::InvalidAttribute(stringify_token(attr)))?;
      Some(AttributeModifier::Validate(value))
    }

    "webidl" => {
      let args = if matches!(attr.meta, Meta::Path(_)) {
        WebIDLArgs {
          default: None,
          options: Vec::new(),
        }
      } else {
        attr.parse_args().map_err(|_| {
          AttributeError::InvalidAttribute(stringify_token(attr))
        })?
      };

      Some(AttributeModifier::WebIDL(args))
    }

    "string" => {
      if matches!(attr.meta, Meta::Path(_)) {
        Some(AttributeModifier::String(StringMode::Default))
      } else if attr
        .parse_args::<Ident>()
        .is_ok_and(|mode| mode == "onebyte")
      {
        Some(AttributeModifier::String(StringMode::OneByte))
      } else {
        return Err(AttributeError::InvalidAttribute(stringify_token(attr)));
      }
    }

    buf @ "buffer" | buf @ "anybuffer" | buf @ "arraybuffer" => {
      let mode = if matches!(attr.meta, Meta::Path(_)) {
        BufferMode::Default
      } else {
        let ident: Ident = attr.parse_args().map_err(|_| {
          AttributeError::InvalidAttribute(stringify_token(attr))
        })?;
        if ident == "unsafe" {
          BufferMode::Unsafe
        } else if ident == "copy" {
          BufferMode::Copy
        } else if ident == "detach" {
          BufferMode::Detach
        } else {
          return Err(AttributeError::InvalidAttribute(stringify_token(attr)));
        }
      };

      let source = match buf {
        "buffer" => BufferSource::TypedArray,
        "anybuffer" => BufferSource::Any,
        "arraybuffer" => BufferSource::ArrayBuffer,
        _ => unreachable!(),
      };

      Some(AttributeModifier::Buffer(mode, source))
    }

    // async is a keyword and does not work as #[async] so we use #[async_method] instead
    "required" | "rename" | "method" | "getter" | "setter" | "fast"
    | "async_method" | "static_method" | "constructor" | "meta" => {
      Some(AttributeModifier::Ignore)
    }

    "allow" | "doc" | "cfg" => None,
    _ => return Err(AttributeError::InvalidAttribute(stringify_token(attr))),
  };

  Ok(modifier)
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

  syn_match::path_match!(
    &tp,
    std?::ffi?::c_void => Ok(NumericArg::__VOID__),
    _ => Err(ArgError::InvalidNumericType(stringify_token(tp))),
  )
}

#[derive(Copy, Clone, Eq, PartialEq)]
enum TypePathContext {
  None,
  Ref,
  Ptr,
}

/// Parse a raw type into a container + type, allowing us to simplify the typechecks elsewhere in
/// this code.
fn parse_type_path(
  position: Position,
  attrs: Attributes,
  ctx: TypePathContext,
  tp: &TypePath,
) -> Result<ParsedTypeContainer, ArgError> {
  use ParsedType::*;
  use ParsedTypeContainer::*;

  let res = match parse_numeric_type(&tp.path) {
    Ok(numeric) => CBare(TNumeric(numeric)),
    _ => {
      syn_match::path_match!(&tp.path,
        std?::str?::String => Ok(CBare(TString(Strings::String))),
        // Note that the reference is checked below
        std?::str?::str => Ok(CBare(TString(Strings::RefStr))),
        std?::borrow?::Cow<'_, str> | std?::borrow?::Cow<str> => Ok(CBare(TString(Strings::CowStr))),
        std?::borrow?::Cow<'_, [u8]> | std?::borrow?::Cow<[u8]> => Ok(CBare(TString(Strings::CowByte))),
        std?::vec?::Vec<::$ty> => {
          if let Some(AttributeModifier::Buffer(_, _)) = attrs.primary {
            Ok(CBare(TBuffer(BufferType::Vec(parse_numeric_type(ty)?))))
          } else if attrs.primary.is_none() || attrs.primary.as_ref().is_some_and(|primary| matches!(primary, AttributeModifier::V8Slow)) {
            Ok(CUnknown(Type::Path(tp.clone()), matches!(attrs.primary, Some(AttributeModifier::V8Slow))))
          } else {
            Err(ArgError::InvalidAttributeType("buffer", stringify_token(tp)))
          }
        },
        std?::boxed?::Box<[$ty]> => {
          if let Type::Path(tp) = ty {
            Ok(CBare(TBuffer(BufferType::BoxSlice(parse_numeric_type(&tp.path)?))))
          } else {
            Err(ArgError::InvalidNumericType(stringify_token(ty)))
          }
        }
        serde_v8?::V8Slice<::$ty> => Ok(CBare(TBuffer(BufferType::V8Slice(parse_numeric_type(ty)?)))),
        serde_v8?::JsBuffer => Ok(CBare(TBuffer(BufferType::JsBuffer))),
        bytes?::Bytes => Ok(CBare(TBuffer(BufferType::Bytes))),
        bytes?::BytesMut => Ok(CBare(TBuffer(BufferType::BytesMut))),
        OpState => Ok(CBare(TSpecial(Special::OpState))),
        JsRuntimeState => Ok(CBare(TSpecial(Special::JsRuntimeState))),
        v8::Isolate => Ok(CBare(TSpecial(Special::Isolate))),
        v8::PinScope<'_, '_> | v8::PinScope => Ok(CBare(TSpecial(Special::HandleScope))),
        v8::FastApiCallbackOptions => Ok(CBare(TSpecial(Special::FastApiCallbackOptions))),
        v8::Local<'_, v8::$v8> | v8::Local<v8::$v8> => Ok(CV8Local(TV8(parse_v8_type(v8)?))),
        v8::Global<'_, v8::$_v8> | v8::Global<v8::$_v8> => Ok(CUnknown(Type::Path(tp.clone()), matches!(attrs.primary, Some(AttributeModifier::V8Slow)))),
        v8::$v8 => Ok(CBare(TV8(parse_v8_type(v8)?))),
        std?::rc?::Rc<RefCell<$ty>> => Ok(CRcRefCell(TSpecial(parse_type_special(position, attrs.clone(), ty)?))),
        std?::rc?::Rc<$ty> => Ok(CRc(TSpecial(parse_type_special(position, attrs.clone(), ty)?))),
        Option<$ty> => {
          let syn::GenericArgument::Type(ty) = ty else {
            return Err(ArgError::InvalidType(
              stringify_token(ty),
              "for option",
            ))
          };

          match parse_type(position, attrs.clone(), ty)? {
            Arg::Special(special) => Ok(COption(TSpecial(special))),
            Arg::String(string) => Ok(COption(TString(string))),
            Arg::Numeric(numeric, _) => Ok(COption(TNumeric(numeric))),
            Arg::Buffer(buffer, ..) => Ok(COption(TBuffer(buffer))),
            Arg::V8Ref(RefType::Ref, v8) => Ok(COption(TV8(v8))),
            Arg::V8Ref(RefType::Mut, v8) => Ok(COption(TV8Mut(v8))),
            Arg::V8Local(v8) => Ok(COptionV8Local(TV8(v8))),
            _ => Ok(CUnknown(Type::Path(tp.clone()), matches!(attrs.primary, Some(AttributeModifier::V8Slow)))),
          }
        }
        deno_core::$next::$any? => {
          // Stylistically it makes more sense just to import deno_core::v8 and other types at the top of the file
          let next = stringify_token(next);
          let any = any.map(|any| format!("::{}", any.into_token_stream())).unwrap_or_default();
          let instead = format!("{next}{any}");
          Err(ArgError::InvalidDenoCorePrefix(stringify_token(tp), next, instead))
        }
        _ => Ok(CUnknown(Type::Path(tp.clone()), matches!(attrs.primary, Some(AttributeModifier::V8Slow)))),
      )?
    }
  };

  // Ensure that we have the correct reference state. This is a bit awkward but it's
  // the easiest way to work with the 'rules!' macro above.
  match res {
    // OpState and JsRuntimeState appears in both ways
    CBare(TSpecial(Special::OpState | Special::JsRuntimeState)) => {}
    CBare(TSpecial(Special::Isolate)) => {
      if ctx != TypePathContext::Ref {
        return Err(ArgError::MissingReference(stringify_token(tp)));
      }
    }
    CBare(
      TString(Strings::RefStr) | TSpecial(Special::HandleScope) | TV8(_),
    ) => {
      if ctx != TypePathContext::Ref {
        return Err(ArgError::MissingReference(stringify_token(tp)));
      }
    }
    _ => {
      if ctx == TypePathContext::Ref {
        return Err(ArgError::InvalidReference(stringify_token(tp)));
      }
    }
  }

  // TODO(mmastrac): this is a bit awkward, but we need to modify the type container here
  // if this is going to work any other way
  if ctx != TypePathContext::Ptr {
    res.validate_attributes(position, attrs, &tp)?;
  }

  Ok(res)
}

fn parse_v8_type(v8: &syn::PathSegment) -> Result<V8Arg, ArgError> {
  let v8 = v8.ident.to_string();
  V8Arg::try_from(v8.as_str()).map_err(|_| ArgError::InvalidV8Type(v8))
}

fn parse_type_special(
  position: Position,
  attrs: Attributes,
  ty: &syn::GenericArgument,
) -> Result<Special, ArgError> {
  let syn::GenericArgument::Type(ty) = ty else {
    return Err(ArgError::InvalidType(
      stringify_token(ty),
      "for special type",
    ));
  };
  match parse_type(position, attrs, ty)? {
    Arg::Special(special) => Ok(special),
    _ => Err(ArgError::InvalidType(
      stringify_token(ty),
      "for special type",
    )),
  }
}

fn parse_cppgc(
  position: Position,
  ty: &Type,
  proto: bool,
) -> Result<Arg, ArgError> {
  match (position, ty) {
    (Position::Arg, Type::Reference(of)) if of.mutability.is_none() => {
      match &*of.elem {
        Type::Path(of) => {
          Ok(Arg::CppGcResource(proto, stringify_token(&of.path)))
        }
        _ => Err(ArgError::InvalidCppGcType(stringify_token(&of.elem))),
      }
    }
    (Position::Arg, Type::Path(of)) => {
      if let Some(seg) = of.path.segments.first()
        && seg.ident == "Option"
        && let PathArguments::AngleBracketed(args) = &seg.arguments
        && let Some(GenericArgument::Type(ty)) = args.args.first()
      {
        match ty {
          Type::Reference(of) if of.mutability.is_none() => match &*of.elem {
            Type::Path(of) => {
              Ok(Arg::OptionCppGcResource(stringify_token(&of.path)))
            }
            _ => Err(ArgError::InvalidCppGcType(stringify_token(&of.elem))),
          },
          _ => Err(ArgError::ExpectedCppGcReference(stringify_token(ty))),
        }
      } else {
        Err(ArgError::ExpectedCppGcReference(stringify_token(ty)))
      }
    }
    (Position::Arg, _) => {
      Err(ArgError::ExpectedCppGcReference(stringify_token(ty)))
    }
    (Position::RetVal, ty) => match ty {
      Type::Path(tp) => {
        if let Some(seg) = tp.path.segments.first()
          && seg.ident == "Option"
          && let PathArguments::AngleBracketed(args) = &seg.arguments
          && let Some(GenericArgument::Type(Type::Path(path))) =
            args.args.first()
        {
          Ok(Arg::OptionCppGcResource(stringify_token(&path.path)))
        } else {
          Ok(Arg::CppGcResource(proto, stringify_token(&tp.path)))
        }
      }
      Type::Tuple(tuple) if tuple.elems.len() == 2 => {
        match (tuple.elems.get(0).unwrap(), tuple.elems.get(1).unwrap()) {
          (Type::Path(sup), Type::Path(ty)) => Ok(Arg::CppGcProtochain(vec![
            stringify_token(&sup.path),
            stringify_token(&ty.path),
          ])),
          _ => Err(ArgError::InvalidCppGcType(stringify_token(ty))),
        }
      }
      _ => Err(ArgError::InvalidCppGcType(stringify_token(ty))),
    },
  }
}

fn better_alternative_exists(position: Position, of: &TypePath) -> bool {
  // If this type will parse without #[serde]/#[to_v8]/#[from_v8], it is illegal to use this type
  // with #[serde]/#[to_v8]/#[from_v8]
  match parse_type_path(
    position,
    Attributes::default(),
    TypePathContext::None,
    of,
  ) {
    Err(_) | Ok(ParsedTypeContainer::CUnknown(_, _)) => {}
    _ => {
      return true;
    }
  }

  // If this type will parse with #[string], it is illegal to use this type with #[serde]/#[to_v8]/#[from_v8]
  if parse_type_path(position, Attributes::string(), TypePathContext::None, of)
    .is_ok()
  {
    return true;
  }

  false
}

pub(crate) fn parse_type(
  position: Position,
  attrs: Attributes,
  ty: &Type,
) -> Result<Arg, ArgError> {
  use ParsedType::*;
  use ParsedTypeContainer::*;

  if let Some(primary) = attrs.primary.clone() {
    match primary {
      AttributeModifier::Ignore | AttributeModifier::Validate(_) => {
        unreachable!();
      }
      AttributeModifier::Undefined => {
        if position == Position::Arg {
          return Err(ArgError::InvalidAttributePosition(
            primary.name(),
            "return value",
          ));
        }
        return Ok(Arg::VoidUndefined);
      }
      AttributeModifier::VarArgs => {
        if position == Position::RetVal {
          return Err(ArgError::InvalidAttributePosition(
            primary.name(),
            "argument",
          ));
        }

        return Ok(Arg::VarArgs);
      }
      AttributeModifier::CppGcResource => {
        return parse_cppgc(position, ty, false);
      }
      AttributeModifier::CppGcProto => return parse_cppgc(position, ty, true),
      AttributeModifier::Serde | AttributeModifier::WebIDL(_) => {
        let make_arg: Box<dyn Fn(String) -> Arg> = match &primary {
          AttributeModifier::Serde => Box::new(Arg::SerdeV8),
          AttributeModifier::WebIDL(args) => Box::new(move |s| {
            Arg::WebIDL(s, args.options.clone(), args.default.clone())
          }),
          _ => unreachable!(),
        };
        match ty {
          Type::Tuple(of) => return Ok(make_arg(stringify_token(of))),
          Type::Path(of) => {
            if !matches!(primary, AttributeModifier::WebIDL(_))
              && better_alternative_exists(position, of)
            {
              return Err(ArgError::InvalidAttributeType(
                primary.name(),
                stringify_token(ty),
              ));
            }

            if let Some(seg) = of.path.segments.first()
              && seg.ident == "Value"
            {
              let invalid = match &seg.arguments {
                PathArguments::None => true,
                PathArguments::AngleBracketed(args)
                  if args.args.first().is_some_and(|arg| {
                    matches!(arg, GenericArgument::Lifetime(_))
                  }) =>
                {
                  true
                }
                _ => false,
              };

              if invalid {
                if primary == AttributeModifier::Serde {
                  return Err(ArgError::InvalidSerdeType(
                    stringify_token(of),
                    "a fully-qualified type: v8::Value or serde_json::Value",
                  ));
                } else {
                  return Err(ArgError::InvalidAttributeType(
                    primary.name(),
                    stringify_token(of),
                  ));
                }
              }
            }

            return Ok(make_arg(stringify_token(of.path.clone())));
          }
          _ => {
            return Err(ArgError::InvalidAttributeType(
              primary.name(),
              stringify_token(ty),
            ));
          }
        }
      }

      AttributeModifier::String(_)
      | AttributeModifier::Buffer(..)
      | AttributeModifier::V8Slow
      | AttributeModifier::Bigint => {
        // We handle this as part of the normal parsing process
      }
      AttributeModifier::This => {
        if position == Position::RetVal {
          return Err(ArgError::InvalidAttributePosition(
            primary.name(),
            "argument",
          ));
        }
        return Ok(Arg::This);
      }
      AttributeModifier::Number => match ty {
        Type::Path(of) => {
          match parse_type_path(
            position,
            attrs.clone(),
            TypePathContext::None,
            of,
          )? {
            COption(TNumeric(
              n @ (NumericArg::u64
              | NumericArg::usize
              | NumericArg::i64
              | NumericArg::isize),
            )) => return Ok(Arg::OptionNumeric(n, NumericFlag::Number)),
            CBare(TNumeric(
              n @ (NumericArg::u64
              | NumericArg::usize
              | NumericArg::i64
              | NumericArg::isize),
            )) => return Ok(Arg::Numeric(n, NumericFlag::Number)),
            _ => {
              return Err(ArgError::InvalidNumberAttributeType(
                stringify_token(ty),
              ));
            }
          }
        }
        _ => {
          return Err(ArgError::InvalidNumberAttributeType(stringify_token(
            ty,
          )));
        }
      },
      AttributeModifier::Smi => match ty {
        Type::Path(of) => {
          if of.path.segments.first().unwrap().ident == "Option" {
            return Ok(Arg::OptionNumeric(
              NumericArg::__SMI__,
              NumericFlag::None,
            ));
          } else {
            return Ok(Arg::Numeric(NumericArg::__SMI__, NumericFlag::None));
          }
        }
        _ => return Err(ArgError::InvalidSmiType(stringify_token(ty))),
      },
    }
  };

  match ty {
    Type::Tuple(of) => {
      if of.elems.is_empty() {
        Ok(Arg::Void)
      } else {
        match position {
          Position::Arg => Ok(Arg::FromV8(
            stringify_token(ty),
            matches!(attrs.primary, Some(AttributeModifier::V8Slow)),
          )),
          Position::RetVal => Ok(Arg::ToV8(stringify_token(ty))),
        }
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
        Type::Slice(of) => match &*of.elem {
          Type::Path(path) => match parse_numeric_type(&path.path)? {
            NumericArg::__VOID__ => Ok(Arg::External(External::Ptr(mut_type))),
            numeric => {
              let res = CBare(TBuffer(BufferType::Slice(mut_type, numeric)));
              res.validate_attributes(position, attrs.clone(), &of)?;
              Arg::from_parsed(res, position, attrs.clone()).map_err(|_| {
                ArgError::InvalidType(stringify_token(ty), "for slice")
              })
            }
          },
          _ => Err(ArgError::InvalidType(stringify_token(ty), "for slice")),
        },
        Type::Path(of) => {
          match parse_type_path(
            position,
            attrs.clone(),
            TypePathContext::Ref,
            of,
          )? {
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
          }
        }
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
        Type::Path(of) => {
          match parse_type_path(
            position,
            attrs.clone(),
            TypePathContext::Ptr,
            of,
          )? {
            CBare(TNumeric(NumericArg::__VOID__)) => {
              Ok(Arg::External(External::Ptr(mut_type)))
            }
            CBare(TNumeric(numeric)) => {
              let res = CBare(TBuffer(BufferType::Ptr(mut_type, numeric)));
              res.validate_attributes(position, attrs.clone(), &of)?;
              Arg::from_parsed(res, position, attrs.clone()).map_err(|_| {
                ArgError::InvalidType(
                  stringify_token(ty),
                  "for numeric pointer",
                )
              })
            }
            CBare(TSpecial(Special::Isolate)) => {
              Ok(Arg::Special(Special::Isolate))
            }
            _ => Err(ArgError::InvalidType(
              stringify_token(of),
              "for pointer to type path",
            )),
          }
        }
        _ => Err(ArgError::InvalidType(stringify_token(ty), "for pointer")),
      }
    }
    Type::Path(of) => {
      let typath =
        parse_type_path(position, attrs.clone(), TypePathContext::None, of)?;
      if let CBare(TSpecial(Special::Isolate)) = typath {
        return Ok(Arg::Special(Special::Isolate));
      }
      Arg::from_parsed(typath, position, attrs)
        .map_err(|_| ArgError::InvalidType(stringify_token(ty), "for path"))
    }
    _ => Err(ArgError::InvalidType(
      stringify_token(ty),
      "for top-level type",
    )),
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use syn::ItemFn;
  use syn::parse_str;

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
      $( < $( $lifetime_res:lifetime )? $(, $generic_res:ident : $bounds_res:ty )* >)? ( $( $arg_res:expr_2021 ),* ) -> $ret_res:expr_2021 ) => {
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
    for lifetime in sig.lifetimes {
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

    let arg_ty = sig.args.iter().map(|a| a.0.clone()).collect::<Vec<_>>();
    assert_eq!(
      args_expected.replace('\n', " "),
      format!("{:?}", arg_ty)
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
    ($name:ident, $error:expr_2021, $f:item) => {
      #[test]
      pub fn $name() {
        #[allow(unused)]
        use super::ArgError::*;
        #[allow(unused)]
        use super::AttributeError::*;
        #[allow(unused)]
        use super::SignatureError::*;

        let op = stringify!($f);
        // Parse the provided macro input as an ItemFn
        let item_fn = parse_str::<ItemFn>(op)
          .unwrap_or_else(|_| panic!("Failed to parse {op} as a ItemFn"));
        let attrs = item_fn.attrs;
        let error = parse_signature(attrs, item_fn.sig)
          .expect_err("Expected function to fail to parse");
        assert_eq!(format!("{error:?}"), format!("{:?}", $error));
      }
    };
  }

  test!(
    fn op_state_and_number(opstate: &mut OpState, a: u32) -> ();
    (Ref(Mut, OpState), Numeric(u32, None)) -> Value(Void)
  );
  test!(
    fn op_slices(#[buffer] r#in: &[u8], #[buffer] out: &mut [u8]);
    (Buffer(Slice(Ref, u8), Default, TypedArray), Buffer(Slice(Mut, u8), Default, TypedArray)) -> Value(Void)
  );
  test!(
    fn op_pointers(#[buffer] r#in: *const u8, #[buffer] out: *mut u8);
    (Buffer(Ptr(Ref, u8), Default, TypedArray), Buffer(Ptr(Mut, u8), Default, TypedArray)) -> Value(Void)
  );
  test!(
    fn op_arraybuffer(#[arraybuffer] r#in: &[u8]);
    (Buffer(Slice(Ref, u8), Default, ArrayBuffer)) -> Value(Void)
  );
  test!(
    #[serde] fn op_serde(#[serde] input: package::SerdeInputType) -> Result<package::SerdeReturnType, Error>;
    (SerdeV8(package::SerdeInputType)) -> Result(Value(SerdeV8(package::SerdeReturnType)))
  );
  // Note the turbofish syntax here because of macro constraints
  test!(
    #[serde] fn op_serde_option(#[serde] maybe: Option<package::SerdeInputType>) -> Result<Option<package::SerdeReturnType>, Error>;
    (SerdeV8(Option::<package::SerdeInputType>)) -> Result(Value(SerdeV8(Option::<package::SerdeReturnType>)))
  );
  test!(
    #[serde] fn op_serde_tuple(#[serde] input: (A, B)) -> (A, B);
    (SerdeV8((A, B))) -> Value(SerdeV8((A, B)))
  );
  test!(
    fn op_local(input: v8::Local<v8::String>) -> Result<v8::Local<v8::String>, Error>;
    (V8Local(String)) -> Result(Value(V8Local(String)))
  );
  test!(
    fn op_resource(#[smi] rid: ResourceId, #[buffer] buffer: &[u8]);
    (Numeric(__SMI__, None), Buffer(Slice(Ref, u8), Default, TypedArray)) ->  Value(Void)
  );
  test!(
    #[smi] fn op_resource2(#[smi] rid: ResourceId) -> Result<ResourceId, Error>;
    (Numeric(__SMI__, None)) -> Result(Value(Numeric(__SMI__, None)))
  );
  test!(
    fn op_option_numeric_result(state: &mut OpState) -> Result<Option<u32>, JsErrorBox>;
    (Ref(Mut, OpState)) -> Result(Value(OptionNumeric(u32, None)))
  );
  test!(
    #[smi] fn op_option_numeric_smi_result(#[smi] a: Option<u32>) -> Result<Option<u32>, JsErrorBox>;
    (OptionNumeric(__SMI__, None)) -> Result(Value(OptionNumeric(__SMI__, None)))
  );
  test!(
    fn op_ffi_read_f64(state: &mut OpState, ptr: *mut c_void, #[bigint] offset: isize) -> Result<f64, JsErrorBox>;
    (Ref(Mut, OpState), External(Ptr(Mut)), Numeric(isize, None)) -> Result(Value(Numeric(f64, None)))
  );
  test!(
    #[number] fn op_64_bit_number(#[number] offset: isize) -> Result<u64, JsErrorBox>;
    (Numeric(isize, Number)) -> Result(Value(Numeric(u64, Number)))
  );
  test!(
    fn op_ptr_out(ptr: *const c_void) -> *mut c_void;
    (External(Ptr(Ref))) -> Value(External(Ptr(Mut)))
  );
  test!(
    fn op_print(#[string] msg: &str, is_err: bool) -> Result<(), Error>;
    (String(RefStr), Numeric(bool, None)) -> Result(Value(Void))
  );
  test!(
    #[string] fn op_lots_of_strings(#[string] s: String, #[string] s2: Option<String>, #[string] s3: Cow<str>, #[string(onebyte)] s4: Cow<[u8]>) -> String;
    (String(String), OptionString(String), String(CowStr), String(CowByte)) -> Value(String(String))
  );
  test!(
    #[string] fn op_lots_of_option_strings(#[string] s: Option<String>, #[string] s2: Option<&str>, #[string] s3: Option<Cow<str>>) -> Option<String>;
    (OptionString(String), OptionString(RefStr), OptionString(CowStr)) -> Value(OptionString(String))
  );
  test!(
    fn op_scope<'s>(#[string] msg: &'s str);
    <'s> (String(RefStr)) -> Value(Void)
  );
  test!(
    fn op_scope_and_generics<'s, AB, BC>(#[string] msg: &'s str) where AB: some::Trait, BC: OtherTrait;
    <'s, AB: some::Trait, BC: OtherTrait> (String(RefStr)) -> Value(Void)
  );
  test!(
    fn op_generics_static<'s, AB, BC>(#[string] msg: &'s str) where AB: some::Trait + 'static, BC: OtherTrait;
    <'s, AB: some::Trait + 'static, BC: OtherTrait> (String(RefStr)) -> Value(Void)
  );
  test!(
    fn op_v8_types(s: &mut v8::String, sopt: Option<&mut v8::String>, s2: v8::Local<v8::String>);
    (V8Ref(Mut, String), OptionV8Ref(Mut, String), V8Local(String)) -> Value(Void)
  );
  test!(
    fn op_v8_scope<'s>(scope: &mut v8::PinScope<'s, '_>);
    <'s> (Ref(Mut, HandleScope)) -> Value(Void)
  );
  test!(
    fn op_state_rc(state: Rc<RefCell<OpState>>);
    (RcRefCell(OpState)) -> Value(Void)
  );
  test!(
    fn op_state_ref(state: &OpState);
    (Ref(Ref, OpState)) -> Value(Void)
  );
  test!(
    #[buffer] fn op_buffers(#[buffer(copy)] a: Vec<u8>, #[buffer(copy)] b: Box<[u8]>, #[buffer(copy)] c: bytes::Bytes,
      #[buffer] d: V8Slice<u8>, #[buffer] e: JsBuffer, #[buffer(detach)] f: JsBuffer) -> Vec<u8>;
    (Buffer(Vec(u8), Copy, TypedArray), Buffer(BoxSlice(u8), Copy, TypedArray),
      Buffer(Bytes, Copy, TypedArray), Buffer(V8Slice(u8), Default, TypedArray),
      Buffer(JsBuffer, Default, TypedArray), Buffer(JsBuffer, Detach, TypedArray)) -> Value(Buffer(Vec(u8), Default, TypedArray))
  );
  test!(
    #[buffer] fn op_return_bytesmut() -> bytes::BytesMut;
    () -> Value(Buffer(BytesMut, Default, TypedArray))
  );
  test!(
    async fn op_async_void();
    () -> Future(Value(Void))
  );
  test!(
    async fn op_async_result_void() -> Result<()>;
    () -> Future(Result(Value(Void)))
  );
  test!(
    fn op_async_impl_void() -> impl Future<Output = ()>;
    () -> Future(Value(Void))
  );
  test!(
    fn op_async_result_impl_void() -> Result<impl Future<Output = ()>, Error>;
    () -> Result(Future(Value(Void)))
  );
  test!(
    fn op_js_runtime_state_ref(state: &JsRuntimeState);
    (Ref(Ref, JsRuntimeState)) -> Value(Void)
  );
  test!(
    fn op_js_runtime_state_mut(state: &mut JsRuntimeState);
    (Ref(Mut, JsRuntimeState)) -> Value(Void)
  );
  test!(
    fn op_js_runtime_state_rc(state: Rc<JsRuntimeState>);
    (Rc(JsRuntimeState)) -> Value(Void)
  );
  expect_fail!(
    op_isolate_bare,
    ArgError("isolate".into(), MissingReference("v8::Isolate".into())),
    fn f(isolate: v8::Isolate) {}
  );
  test!(
    fn op_isolate_ref(isolate: &v8::Isolate);
    (Ref(Ref, Isolate)) -> Value(Void)
  );
  test!(
    fn op_isolate_mut(isolate: &mut v8::Isolate);
    (Ref(Mut, Isolate)) -> Value(Void)
  );
  test!(
    #[serde]
    async fn op_serde_result_with_comma(
      state: Rc<RefCell<OpState>>,
      #[smi] rid: ResourceId
    ) -> Result<
      ExtremelyLongTypeNameThatForcesEverythingToWrapAndAddsCommas,
      JsErrorBox,
    >;
    (RcRefCell(OpState), Numeric(__SMI__, None)) -> Future(Result(Value(SerdeV8(ExtremelyLongTypeNameThatForcesEverythingToWrapAndAddsCommas))))
  );
  expect_fail!(
    op_cppgc_resource_owned,
    ArgError(
      "resource".into(),
      ExpectedCppGcReference("std::fs::File".into())
    ),
    fn f(#[cppgc] resource: std::fs::File) {}
  );
  expect_fail!(
    op_cppgc_resource_option_owned,
    ArgError(
      "resource".into(),
      ExpectedCppGcReference("std::fs::File".into())
    ),
    fn f(#[cppgc] resource: Option<std::fs::File>) {}
  );
  expect_fail!(
    op_cppgc_resource_invalid_type,
    ArgError(
      "resource".into(),
      InvalidCppGcType("[std :: fs :: File]".into())
    ),
    fn f(#[cppgc] resource: &[std::fs::File]) {}
  );
  expect_fail!(
    op_cppgc_resource_option_invalid_type,
    ArgError(
      "resource".into(),
      InvalidCppGcType("[std :: fs :: File]".into())
    ),
    fn f(#[cppgc] resource: Option<&[std::fs::File]>) {}
  );

  // Args

  expect_fail!(
    op_with_bad_string1,
    ArgError("s".into(), MissingAttribute("string", "str".into())),
    fn f(s: &str) {}
  );
  expect_fail!(
    op_with_bad_string2,
    ArgError("s".into(), MissingAttribute("string", "String".into())),
    fn f(s: String) {}
  );
  expect_fail!(
    op_with_bad_string3,
    ArgError("s".into(), MissingAttribute("string", "Cow<str>".into())),
    fn f(s: Cow<str>) {}
  );
  expect_fail!(
    op_with_invalid_string,
    ArgError("x".into(), InvalidAttributeType("string", "u32".into())),
    fn f(#[string] x: u32) {}
  );
  expect_fail!(
    op_with_invalid_buffer,
    ArgError("x".into(), InvalidAttributeType("buffer", "u32".into())),
    fn f(#[buffer] x: u32) {}
  );
  expect_fail!(
    op_with_bad_attr,
    RetError(super::RetError::AttributeError(InvalidAttribute(
      "#[badattr]".into()
    ))),
    #[badattr]
    fn f() {}
  );
  expect_fail!(
    op_with_bad_attr2,
    ArgError(
      "a".into(),
      AttributeError(InvalidAttribute("#[badattr]".into()))
    ),
    fn f(#[badattr] a: u32) {}
  );
  expect_fail!(
    op_duplicate_js_runtime_state,
    InvalidMultipleJsRuntimeState,
    fn f(s1: &JsRuntimeState, s2: &mut JsRuntimeState) {}
  );
  expect_fail!(
    op_extra_deno_core_v8,
    ArgError(
      "a".into(),
      InvalidDenoCorePrefix(
        "deno_core::v8::Function".into(),
        "v8".into(),
        "v8::Function".into()
      )
    ),
    fn f(a: &deno_core::v8::Function) {}
  );
  expect_fail!(
    op_extra_deno_core_opstate,
    ArgError(
      "a".into(),
      InvalidDenoCorePrefix(
        "deno_core::OpState".into(),
        "OpState".into(),
        "OpState".into()
      )
    ),
    fn f(a: &deno_core::OpState) {}
  );

  // Generics

  expect_fail!(
    op_with_lifetime_bounds,
    LifetimesMayNotHaveBounds("'a".into()),
    fn f<'a: 'b, 'b>() {}
  );
  expect_fail!(
    op_with_missing_bounds,
    GenericBoundCardinality("B".into()),
    fn f<'a, B>() {}
  );
  expect_fail!(
    op_with_duplicate_bounds,
    GenericBoundCardinality("B".into()),
    fn f<'a, B: Trait>()
    where
      B: Trait,
    {
    }
  );
  expect_fail!(
    op_with_extra_bounds,
    WherePredicateMustAppearInGenerics("C".into()),
    fn f<'a, B>()
    where
      B: Trait,
      C: Trait,
    {
    }
  );

  expect_fail!(
    op_with_bad_serde_string,
    ArgError("s".into(), InvalidAttributeType("serde", "String".into())),
    fn f(#[serde] s: String) {}
  );
  expect_fail!(
    op_with_bad_serde_str,
    ArgError("s".into(), InvalidAttributeType("serde", "&str".into())),
    fn f(#[serde] s: &str) {}
  );
}
