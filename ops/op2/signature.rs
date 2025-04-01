// Copyright 2018-2025 the Deno authors. MIT license.

use proc_macro_rules::rules;
use proc_macro2::Ident;
use proc_macro2::Span;
use proc_macro2::TokenStream;
use quote::ToTokens;
use quote::TokenStreamExt;
use quote::format_ident;
use quote::quote;
use std::collections::BTreeMap;
use syn::parse::Parse;
use syn::parse::ParseStream;

use strum::IntoEnumIterator;
use strum::IntoStaticStr;
use strum_macros::EnumIter;
use strum_macros::EnumString;
use syn::AttrStyle;
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
pub struct WebIDLPairs(pub Ident, pub syn::Expr);
impl PartialEq for WebIDLPairs {
  fn eq(&self, other: &Self) -> bool {
    self.0 == other.0
  }
}
impl Eq for WebIDLPairs {}

#[derive(Clone, Debug)]
pub struct WebIDLDefault(pub syn::Expr);
impl PartialEq for WebIDLDefault {
  fn eq(&self, _other: &Self) -> bool {
    true
  }
}
impl Eq for WebIDLDefault {}

/// Args are not a 1:1 mapping with Rust types, rather they represent broad classes of types that
/// tend to have similar argument handling characteristics. This may need one more level of indirection
/// given how many of these types have option variants, however.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Arg {
  Void,
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
  OptionV8Global(V8Arg),
  V8Local(V8Arg),
  V8Global(V8Arg),
  OptionV8Ref(RefType, V8Arg),
  V8Ref(RefType, V8Arg),
  Numeric(NumericArg, NumericFlag),
  SerdeV8(String),
  State(RefType, String),
  OptionState(RefType, String),
  CppGcResource(bool, String),
  OptionCppGcResource(String),
  CppGcProtochain(Vec<String>),
  FromV8(String),
  ToV8(String),
  WebIDL(String, Vec<WebIDLPairs>, Option<WebIDLDefault>),
  VarArgs,
  This,
}

impl Arg {
  fn from_parsed(
    parsed: ParsedTypeContainer,
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
      COptionV8Global(TV8(v8)) => Ok(Arg::OptionV8Global(v8)),
      COption(TV8(v8)) => Ok(Arg::OptionV8Ref(RefType::Ref, v8)),
      COption(TV8Mut(v8)) => Ok(Arg::OptionV8Ref(RefType::Mut, v8)),
      CV8Local(TV8(v8)) => Ok(Arg::V8Local(v8)),
      CV8Global(TV8(v8)) => Ok(Arg::V8Global(v8)),
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
        | Special::HandleScope,
      ) => true,
      Self::RcRefCell(
        Special::FastApiCallbackOptions
        | Special::OpState
        | Special::JsRuntimeState
        | Special::HandleScope,
      ) => true,
      Self::State(..) | Self::OptionState(..) => true,
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
      Arg::V8Global(v8) => quote!(deno_core::v8::Global<deno_core::v8::#v8>),
      Arg::OptionV8Ref(RefType::Ref, v8) => {
        quote!(::std::option::Option<&deno_core::v8::#v8>)
      }
      Arg::OptionV8Ref(RefType::Mut, v8) => {
        quote!(::std::option::Option<&mut deno_core::v8::#v8>)
      }
      Arg::OptionV8Local(v8) => {
        quote!(::std::option::Option<deno_core::v8::Local<deno_core::v8::#v8>>)
      }
      Arg::OptionV8Global(v8) => {
        quote!(::std::option::Option<deno_core::v8::Global<deno_core::v8::#v8>>)
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
        | Arg::OptionBuffer(..)
        | Arg::OptionState(..)
        | Arg::OptionCppGcResource(..)
    )
  }

  /// Return the `Some` part of this `Option` type, or `None` if it is not an `Option`.
  pub fn some_type(&self) -> Option<Arg> {
    Some(match self {
      Arg::OptionV8Ref(r, t) => Arg::V8Ref(*r, *t),
      Arg::OptionV8Local(t) => Arg::V8Local(*t),
      Arg::OptionV8Global(t) => Arg::V8Global(*t),
      Arg::OptionNumeric(t, flag) => Arg::Numeric(*t, *flag),
      Arg::Option(t) => Arg::Special(t.clone()),
      Arg::OptionString(t) => Arg::String(*t),
      Arg::OptionBuffer(t, m, s) => Arg::Buffer(*t, *m, *s),
      Arg::OptionState(r, t) => Arg::State(*r, t.clone()),
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
          Arg::Void | Arg::Numeric(..) => ArgSlowRetval::RetVal,
          Arg::External(_) => ArgSlowRetval::V8Local,
          // Fast return value path for empty strings
          Arg::String(_) => ArgSlowRetval::RetValFallible,
          Arg::SerdeV8(_) => ArgSlowRetval::V8LocalFalliable,
          Arg::ToV8(_) => ArgSlowRetval::V8LocalFalliable,
          // No scope required for these
          Arg::V8Local(_) => ArgSlowRetval::V8LocalNoScope,
          Arg::V8Global(_) => ArgSlowRetval::V8Local,
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
  /// This type should be converted with `ToV8``
  ToV8,
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
pub enum ParsedTypeContainer {
  CBare(ParsedType),
  COption(ParsedType),
  CRc(ParsedType),
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
pub enum RetVal {
  /// An op that can never fail.
  Infallible(Arg, bool),
  /// An op returning Result<Something, ...>
  Result(Arg, bool),
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
      Future(..)
        | FutureResult(..)
        | ResultFuture(..)
        | ResultFutureResult(..)
        | Infallible(.., true)
        | Result(.., true)
    )
  }

  /// If this function returns a `Result<T, E>` (including if `T` is a `Future`), return `Some(T)`.
  pub fn unwrap_result(&self) -> Option<RetVal> {
    use RetVal::*;
    Some(match self {
      Result(arg, false) => Infallible(arg.clone(), false),
      ResultFuture(arg) => Future(arg.clone()),
      ResultFutureResult(arg) => FutureResult(arg.clone()),
      _ => return None,
    })
  }

  pub fn arg(&self) -> &Arg {
    use RetVal::*;
    match self {
      Infallible(arg, ..)
      | Result(arg, ..)
      | Future(arg)
      | FutureResult(arg)
      | ResultFuture(arg)
      | ResultFutureResult(arg) => arg,
    }
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
  /// #[to_v8], for types that impl `ToV8`
  ToV8,
  /// #[from_v8] for types that impl `FromV8`
  FromV8,
  /// #[webidl], for types that impl `WebIdlConverter`
  WebIDL {
    options: Vec<WebIDLPairs>,
    default: Option<WebIDLDefault>,
  },
  /// #[smi], for non-integral ID types representing small integers (-2³¹ and 2³¹-1 on 64-bit platforms,
  /// see https://medium.com/fhinkel/v8-internals-how-small-is-a-small-integer-e0badc18b6da).
  Smi,
  /// #[string], for strings.
  String(StringMode),
  /// #[state], for automatic OpState extraction.
  State,
  /// #[buffer], for buffers.
  Buffer(BufferMode, BufferSource),
  /// #[global], for [`v8::Global`]s
  Global,
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
}

impl AttributeModifier {
  fn name(&self) -> &'static str {
    match self {
      AttributeModifier::ToV8 => "to_v8",
      AttributeModifier::FromV8 => "from_v8",
      AttributeModifier::Bigint => "bigint",
      AttributeModifier::Number => "number",
      AttributeModifier::Buffer(..) => "buffer",
      AttributeModifier::Smi => "smi",
      AttributeModifier::Serde => "serde",
      AttributeModifier::WebIDL { .. } => "webidl",
      AttributeModifier::String(_) => "string",
      AttributeModifier::State => "state",
      AttributeModifier::Global => "global",
      AttributeModifier::CppGcResource => "cppgc",
      AttributeModifier::CppGcProto => "proto",
      AttributeModifier::Ignore => "ignore",
      AttributeModifier::VarArgs => "varargs",
      AttributeModifier::This => "this",
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
    "Invalid generic: '{0}' Only simple generics bounds are allowed (eg: T: Trait)"
  )]
  InvalidGeneric(String),
  #[error(
    "Invalid predicate: '{0}' Only simple where predicates are allowed (eg: T: Trait)"
  )]
  InvalidWherePredicate(String),
  #[error(
    "State may be either a single OpState parameter, one mutable #[state], or multiple immultiple #[state]s"
  )]
  InvalidOpStateCombination,
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
  #[error("Invalid self argument")]
  InvalidSelf,
  #[error("Invalid argument type: {0} ({1})")]
  InvalidType(String, &'static str),
  #[error("Invalid numeric argument type: {0}")]
  InvalidNumericType(String),
  #[error("Invalid numeric #[smi] argument type: {0}")]
  InvalidSmiType(String),
  #[error(
    "Invalid argument type path (should this be #[smi], #[serde], or #[to_v8]?): {0}"
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
  #[error("Cannot use #[number] for type: {0}")]
  InvalidNumberAttributeType(String),
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

#[derive(Clone, Default)]
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

struct MetadataPair {
  key: Ident,
  _eq: syn::Token![=],
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
    let pairs = input.parse_terminated(MetadataPair::parse, syn::Token![,])?;
    Ok(Self { pairs })
  }
}

struct MetadataPairs {
  pairs: syn::punctuated::Punctuated<MetadataPair, syn::Token![,]>,
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
  is_fake_async: bool,
  attributes: Vec<Attribute>,
  signature: Signature,
) -> Result<ParsedSignature, SignatureError> {
  let mut args = vec![];
  let mut names = vec![];
  for input in signature.inputs {
    let name = match &input {
      // Skip receiver
      FnArg::Receiver(_) => continue,
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
    is_fake_async,
    parse_attributes(&attributes).map_err(RetError::AttributeError)?,
    &signature.output,
  )?;
  let lifetime = parse_lifetime(&signature.generics)?;
  let generic_bounds = parse_generics(&signature.generics)?;

  let mut has_opstate = false;
  let mut has_mut_state = false;
  let mut has_ref_state = false;

  let mut jsruntimestate_count = 0;

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
      Arg::Ref(_, Special::JsRuntimeState) => {
        jsruntimestate_count += 1;
      }
      Arg::RcRefCell(Special::JsRuntimeState) => {
        jsruntimestate_count += 1;
      }
      _ => {}
    }
  }

  // Ensure that either zero or one and only one of these are true
  if has_opstate as u8 + has_mut_state as u8 + has_ref_state as u8 > 1 {
    return Err(SignatureError::InvalidOpStateCombination);
  }

  // Ensure that there is at most one JsRuntimeState
  if jsruntimestate_count > 1 {
    return Err(SignatureError::InvalidMultipleJsRuntimeState);
  }

  let metadata = parse_metadata(&attributes)?;

  Ok(ParsedSignature {
    args,
    names,
    ret_val,
    lifetime,
    generic_bounds,
    metadata,
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
      if attr == AttributeModifier::Ignore {
        continue;
      }
      attrs.push(attr)
    }
  }

  if attrs.is_empty() {
    return Ok(Attributes::default());
  }
  Ok(Attributes {
    primary: Some((*attrs.first().unwrap()).clone()),
  })
}

/// Is this a special attribute that we understand?
pub fn is_attribute_special(attr: &Attribute) -> bool {
  parse_attribute(attr)
      .unwrap_or_default()
      .and_then(|attr| match attr {
        AttributeModifier::Ignore => None,
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
  let tokens = attr.into_token_stream();
  if matches!(attr.style, AttrStyle::Inner(_)) {
    return Err(AttributeError::InvalidInnerAttribute);
  }
  let res = std::panic::catch_unwind(|| {
    rules!(tokens => {
      (#[bigint]) => Some(AttributeModifier::Bigint),
      (#[number]) => Some(AttributeModifier::Number),
      (#[serde]) => Some(AttributeModifier::Serde),
      (#[webidl]) => Some(AttributeModifier::WebIDL { options: vec![],default: None }),
      (#[webidl($(default = $default:expr)?$($(, )?options($($key:ident = $value:expr),*))?)]) => Some(AttributeModifier::WebIDL { options: key.map(|key| key.into_iter().zip(value.unwrap().into_iter()).map(|v| WebIDLPairs(v.0, v.1)).collect()).unwrap_or_default(), default: default.map(WebIDLDefault) }),
      (#[smi]) => Some(AttributeModifier::Smi),
      (#[string]) => Some(AttributeModifier::String(StringMode::Default)),
      (#[string(onebyte)]) => Some(AttributeModifier::String(StringMode::OneByte)),
      (#[state]) => Some(AttributeModifier::State),
      (#[varargs]) => Some(AttributeModifier::VarArgs),
      (#[buffer]) => Some(AttributeModifier::Buffer(BufferMode::Default, BufferSource::TypedArray)),
      (#[buffer(unsafe)]) => Some(AttributeModifier::Buffer(BufferMode::Unsafe, BufferSource::TypedArray)),
      (#[buffer(copy)]) => Some(AttributeModifier::Buffer(BufferMode::Copy, BufferSource::TypedArray)),
      (#[buffer(detach)]) => Some(AttributeModifier::Buffer(BufferMode::Detach, BufferSource::TypedArray)),
      (#[anybuffer]) => Some(AttributeModifier::Buffer(BufferMode::Default, BufferSource::Any)),
      (#[anybuffer(unsafe)]) => Some(AttributeModifier::Buffer(BufferMode::Unsafe, BufferSource::Any)),
      (#[anybuffer(copy)]) => Some(AttributeModifier::Buffer(BufferMode::Copy, BufferSource::Any)),
      (#[anybuffer(detach)]) => Some(AttributeModifier::Buffer(BufferMode::Detach, BufferSource::Any)),
      (#[arraybuffer]) => Some(AttributeModifier::Buffer(BufferMode::Default, BufferSource::ArrayBuffer)),
      (#[arraybuffer(unsafe)]) => Some(AttributeModifier::Buffer(BufferMode::Unsafe, BufferSource::ArrayBuffer)),
      (#[arraybuffer(copy)]) => Some(AttributeModifier::Buffer(BufferMode::Copy, BufferSource::ArrayBuffer)),
      (#[arraybuffer(detach)]) => Some(AttributeModifier::Buffer(BufferMode::Detach, BufferSource::ArrayBuffer)),
      (#[global]) => Some(AttributeModifier::Global),
      (#[this]) => Some(AttributeModifier::This),
      (#[cppgc]) => Some(AttributeModifier::CppGcResource),
      (#[proto]) => Some(AttributeModifier::CppGcProto),
      (#[to_v8]) => Some(AttributeModifier::ToV8),
      (#[from_v8]) => Some(AttributeModifier::FromV8),
      (#[required ($_attr:literal)]) => Some(AttributeModifier::Ignore),
      (#[rename ($_attr:literal)]) => Some(AttributeModifier::Ignore),
      (#[method ($_attr:literal)]) => Some(AttributeModifier::Ignore),
      (#[method]) => Some(AttributeModifier::Ignore),
      (#[getter]) => Some(AttributeModifier::Ignore),
      (#[setter]) => Some(AttributeModifier::Ignore),
      (#[fast]) => Some(AttributeModifier::Ignore),
      // async is a keyword and does not work as #[async] so we use #[async_method] instead
      (#[async_method]) => Some(AttributeModifier::Ignore),
      (#[static_method]) => Some(AttributeModifier::Ignore),
      (#[constructor]) => Some(AttributeModifier::Ignore),
      (#[allow ($_rule:path)]) => None,
      (#[doc = $_attr:literal]) => None,
      (#[cfg $_cfg:tt]) => None,
      (#[meta ($($_key: ident = $_value: literal),*)]) => Some(AttributeModifier::Ignore),
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

  let Ok(Some(res)) = std::panic::catch_unwind(|| {
    rules!(tp.into_token_stream() => {
      ( $( std :: ffi :: )? c_void ) => Some(NumericArg::__VOID__),
      ( $_ty:ty ) => None,
    })
  }) else {
    return Err(ArgError::InvalidNumericType(stringify_token(tp)));
  };

  Ok(res)
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

  let tokens = tp.clone().into_token_stream();
  let res = match parse_numeric_type(&tp.path) {
    Ok(numeric) => CBare(TNumeric(numeric)),
    _ => {
      std::panic::catch_unwind(|| {
      rules!(tokens => {
      ( $( std :: str  :: )? String ) => {
        Ok(CBare(TString(Strings::String)))
      }
      // Note that the reference is checked below
      ( $( std :: str :: )? str ) => {
        Ok(CBare(TString(Strings::RefStr)))
      }
      ( $( std :: borrow :: )? Cow < $( $_lt:lifetime , )? str $(,)? > ) => {
        Ok(CBare(TString(Strings::CowStr)))
      }
      ( $( std :: borrow :: )? Cow < $( $_lt:lifetime , )? [ u8 ] $(,)? > ) => {
        Ok(CBare(TString(Strings::CowByte)))
      }
      ( $( std :: vec ::)? Vec < $ty:path $(,)? > ) => {
        Ok(CBare(TBuffer(BufferType::Vec(parse_numeric_type(&ty)?))))
      }
      ( $( std :: boxed ::)? Box < [ $ty:path ] $(,)? > ) => {
        Ok(CBare(TBuffer(BufferType::BoxSlice(parse_numeric_type(&ty)?))))
      }
      ( $( serde_v8 :: )? V8Slice < $ty:path $(,)? > ) => {
        Ok(CBare(TBuffer(BufferType::V8Slice(parse_numeric_type(&ty)?))))
      }
      ( $( serde_v8 :: )? JsBuffer ) => {
        Ok(CBare(TBuffer(BufferType::JsBuffer)))
      }
      ( $( bytes :: )? Bytes ) => {
        Ok(CBare(TBuffer(BufferType::Bytes)))
      }
      ( $( bytes :: )? BytesMut ) => {
        Ok(CBare(TBuffer(BufferType::BytesMut)))
      }
      ( OpState ) => Ok(CBare(TSpecial(Special::OpState))),
      ( JsRuntimeState ) => Ok(CBare(TSpecial(Special::JsRuntimeState))),
      ( v8 :: Isolate ) => Ok(CBare(TSpecial(Special::Isolate))),
      ( v8 :: HandleScope $( < $_scope:lifetime >)? ) => Ok(CBare(TSpecial(Special::HandleScope))),
      ( v8 :: FastApiCallbackOptions ) => Ok(CBare(TSpecial(Special::FastApiCallbackOptions))),
      ( v8 :: Local < $( $_scope:lifetime , )? v8 :: $v8:ident $(,)? >) => Ok(CV8Local(TV8(parse_v8_type(&v8)?))),
      ( v8 :: Global < $( $_scope:lifetime , )? v8 :: $v8:ident $(,)? >) => Ok(CV8Global(TV8(parse_v8_type(&v8)?))),
      ( v8 :: $v8:ident ) => Ok(CBare(TV8(parse_v8_type(&v8)?))),
      ( $( std :: rc :: )? Rc < RefCell < $ty:ty $(,)? > $(,)? > ) => Ok(CRcRefCell(TSpecial(parse_type_special(position, attrs.clone(), &ty)?))),
      ( $( std :: rc :: )? Rc < $ty:ty $(,)? > ) => Ok(CRc(TSpecial(parse_type_special(position, attrs.clone(), &ty)?))),
      ( Option < $ty:ty $(,)? > ) => {
        match parse_type(position, attrs.clone(), &ty)? {
          Arg::Special(special) => Ok(COption(TSpecial(special))),
          Arg::String(string) => Ok(COption(TString(string))),
          Arg::Numeric(numeric, _) => Ok(COption(TNumeric(numeric))),
          Arg::Buffer(buffer, ..) => Ok(COption(TBuffer(buffer))),
          Arg::V8Ref(RefType::Ref, v8) => Ok(COption(TV8(v8))),
          Arg::V8Ref(RefType::Mut, v8) => Ok(COption(TV8Mut(v8))),
          Arg::V8Local(v8) => Ok(COptionV8Local(TV8(v8))),
          Arg::V8Global(v8) => Ok(COptionV8Global(TV8(v8))),
          _ => Err(ArgError::InvalidType(stringify_token(ty), "for option"))
        }
      }
      ( deno_core :: $next:ident $(:: $any:ty)? ) => {
        // Stylistically it makes more sense just to import deno_core::v8 and other types at the top of the file
        let any = any.map(|any| format!("::{}", any.into_token_stream())).unwrap_or_default();
        let instead = format!("{next}{any}");
        Err(ArgError::InvalidDenoCorePrefix(stringify_token(tp), stringify_token(next), instead))
      }
      ( $any:ty ) => {
        Err(ArgError::InvalidTypePath(stringify_token(any)))
      }
    })
    }).map_err(|e| ArgError::InternalError(format!("parse_type_path {e:?}")))??
    }
  };

  // Ensure that we have the correct reference state. This is a bit awkward but it's
  // the easiest way to work with the 'rules!' macro above.
  match res {
    // OpState and JsRuntimeState appears in both ways
    CBare(TSpecial(Special::OpState | Special::JsRuntimeState)) => {}
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
          (Option< $ty:ty $(,)? >) => ty,
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
      rules!(of.to_token_stream() => {
        ( Option < $ty:ty $(,)? > ) => {
          match ty {
            Type::Reference(of) if of.mutability.is_none() => {
              match &*of.elem {
                Type::Path(of) => Ok(Arg::OptionCppGcResource(stringify_token(&of.path))),
                _ => Err(ArgError::InvalidCppGcType(stringify_token(&of.elem))),
              }
            }
            _ => Err(ArgError::ExpectedCppGcReference(stringify_token(ty))),
          }
        }
        ( $_ty:ty ) => Err(ArgError::ExpectedCppGcReference(stringify_token(ty))),
      })
    }
    (Position::Arg, _) => {
      Err(ArgError::ExpectedCppGcReference(stringify_token(ty)))
    }
    (Position::RetVal, ty) => {
      rules!(ty.to_token_stream() => {
        ( Option < $ty:ty $(,)? > ) => {
          match ty {
            Type::Path(of) => Ok(Arg::OptionCppGcResource(stringify_token(&of.path))),
            _ => Err(ArgError::InvalidCppGcType(stringify_token(ty))),
          }
        }
        ( ($sup:ty, $ty:ty) ) => match (sup, &ty) {
           (Type::Path(sup), Type::Path(ty)) => Ok(Arg::CppGcProtochain(vec![stringify_token(&sup.path), stringify_token(&ty.path)])),
           _ => Err(ArgError::InvalidCppGcType(stringify_token(ty))),
        },
        ( $ty:ty ) => match ty {
          Type::Path(of) => Ok(Arg::CppGcResource(proto, stringify_token(&of.path))),
          _ => Err(ArgError::InvalidCppGcType(stringify_token(ty))),
        },
      })
    }
  }
}

fn better_alternative_exists(position: Position, of: &TypePath) -> bool {
  // If this type will parse without #[serde]/#[to_v8]/#[from_v8], it is illegal to use this type
  // with #[serde]/#[to_v8]/#[from_v8]
  if parse_type_path(position, Attributes::default(), TypePathContext::None, of)
    .is_ok()
  {
    return true;
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

  if let Some(primary) = attrs.clone().primary {
    match primary {
      AttributeModifier::Ignore => {
        unreachable!();
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
      AttributeModifier::FromV8 if position == Position::RetVal => {
        return Err(ArgError::InvalidAttributePosition(
          primary.name(),
          "argument",
        ));
      }
      AttributeModifier::ToV8 if position == Position::Arg => {
        return Err(ArgError::InvalidAttributePosition(
          primary.name(),
          "return value",
        ));
      }
      AttributeModifier::Serde
      | AttributeModifier::FromV8
      | AttributeModifier::ToV8
      | AttributeModifier::WebIDL { .. } => {
        let make_arg: Box<dyn Fn(String) -> Arg> = match &primary {
          AttributeModifier::Serde => Box::new(Arg::SerdeV8),
          AttributeModifier::FromV8 => Box::new(Arg::FromV8),
          AttributeModifier::ToV8 => Box::new(Arg::ToV8),
          AttributeModifier::WebIDL { options, default } => {
            Box::new(move |s| Arg::WebIDL(s, options.clone(), default.clone()))
          }
          _ => unreachable!(),
        };
        match ty {
          Type::Tuple(of) => return Ok(make_arg(stringify_token(of))),
          Type::Path(of) => {
            if !matches!(primary, AttributeModifier::WebIDL { .. })
              && better_alternative_exists(position, of)
            {
              return Err(ArgError::InvalidAttributeType(
                primary.name(),
                stringify_token(ty),
              ));
            }

            let ty = of.into_token_stream();
            if let Ok(Some(err)) = std::panic::catch_unwind(|| {
              rules!(ty => {
                ( Value $( < $_lifetime:lifetime $(,)? >)? ) => Some("a fully-qualified type: v8::Value or serde_json::Value"),
                ( $_ty:ty ) => None,
              })
            }) {
              if primary == AttributeModifier::Serde {
                return Err(ArgError::InvalidSerdeType(
                  stringify_token(ty),
                  err,
                ));
              } else {
                return Err(ArgError::InvalidAttributeType(
                  primary.name(),
                  stringify_token(ty),
                ));
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

      AttributeModifier::State => {
        return parse_type_state(ty);
      }
      AttributeModifier::String(_)
      | AttributeModifier::Buffer(..)
      | AttributeModifier::Bigint
      | AttributeModifier::Global => {
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
          let is_option = rules!(of.into_token_stream() => {
            ( Option < $_ty:ty $(,)? > ) => true,
            ( $_ty:ty ) => false,
          });
          if is_option {
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
        Type::Slice(of) => match &*of.elem {
          Type::Path(path) => match parse_numeric_type(&path.path)? {
            NumericArg::__VOID__ => Ok(Arg::External(External::Ptr(mut_type))),
            numeric => {
              let res = CBare(TBuffer(BufferType::Slice(mut_type, numeric)));
              res.validate_attributes(position, attrs.clone(), &of)?;
              Arg::from_parsed(res, attrs.clone()).map_err(|_| {
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
              Arg::from_parsed(res, attrs.clone()).map_err(|_| {
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
    Type::Path(of) => Arg::from_parsed(
      parse_type_path(position, attrs.clone(), TypePathContext::None, of)?,
      attrs,
    )
    .map_err(|_| ArgError::InvalidType(stringify_token(ty), "for path")),
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
    let sig =
      parse_signature(false, attrs, item_fn.sig).unwrap_or_else(|err| {
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
        let error = parse_signature(false, attrs, item_fn.sig)
          .expect_err("Expected function to fail to parse");
        assert_eq!(format!("{error:?}"), format!("{:?}", $error));
      }
    };
  }

  test!(
    fn op_state_and_number(opstate: &mut OpState, a: u32) -> ();
    (Ref(Mut, OpState), Numeric(u32, None)) -> Infallible(Void, false)
  );
  test!(
    fn op_slices(#[buffer] r#in: &[u8], #[buffer] out: &mut [u8]);
    (Buffer(Slice(Ref, u8), Default, TypedArray), Buffer(Slice(Mut, u8), Default, TypedArray)) -> Infallible(Void, false)
  );
  test!(
    fn op_pointers(#[buffer] r#in: *const u8, #[buffer] out: *mut u8);
    (Buffer(Ptr(Ref, u8), Default, TypedArray), Buffer(Ptr(Mut, u8), Default, TypedArray)) -> Infallible(Void, false)
  );
  test!(
    fn op_arraybuffer(#[arraybuffer] r#in: &[u8]);
    (Buffer(Slice(Ref, u8), Default, ArrayBuffer)) -> Infallible(Void, false)
  );
  test!(
    #[serde] fn op_serde(#[serde] input: package::SerdeInputType) -> Result<package::SerdeReturnType, Error>;
    (SerdeV8(package::SerdeInputType)) -> Result(SerdeV8(package::SerdeReturnType), false)
  );
  // Note the turbofish syntax here because of macro constraints
  test!(
    #[serde] fn op_serde_option(#[serde] maybe: Option<package::SerdeInputType>) -> Result<Option<package::SerdeReturnType>, Error>;
    (SerdeV8(Option::<package::SerdeInputType>)) -> Result(SerdeV8(Option::<package::SerdeReturnType>), false)
  );
  test!(
    #[serde] fn op_serde_tuple(#[serde] input: (A, B)) -> (A, B);
    (SerdeV8((A, B))) -> Infallible(SerdeV8((A, B)), false)
  );
  test!(
    fn op_local(input: v8::Local<v8::String>) -> Result<v8::Local<v8::String>, Error>;
    (V8Local(String)) -> Result(V8Local(String), false)
  );
  test!(
    fn op_resource(#[smi] rid: ResourceId, #[buffer] buffer: &[u8]);
    (Numeric(__SMI__, None), Buffer(Slice(Ref, u8), Default, TypedArray)) ->  Infallible(Void, false)
  );
  test!(
    #[smi] fn op_resource2(#[smi] rid: ResourceId) -> Result<ResourceId, Error>;
    (Numeric(__SMI__, None)) -> Result(Numeric(__SMI__, None), false)
  );
  test!(
    fn op_option_numeric_result(state: &mut OpState) -> Result<Option<u32>, JsErrorBox>;
    (Ref(Mut, OpState)) -> Result(OptionNumeric(u32, None), false)
  );
  test!(
    #[smi] fn op_option_numeric_smi_result(#[smi] a: Option<u32>) -> Result<Option<u32>, JsErrorBox>;
    (OptionNumeric(__SMI__, None)) -> Result(OptionNumeric(__SMI__, None), false)
  );
  test!(
    fn op_ffi_read_f64(state: &mut OpState, ptr: *mut c_void, #[bigint] offset: isize) -> Result<f64, JsErrorBox>;
    (Ref(Mut, OpState), External(Ptr(Mut)), Numeric(isize, None)) -> Result(Numeric(f64, None), false)
  );
  test!(
    #[number] fn op_64_bit_number(#[number] offset: isize) -> Result<u64, JsErrorBox>;
    (Numeric(isize, Number)) -> Result(Numeric(u64, Number), false)
  );
  test!(
    fn op_ptr_out(ptr: *const c_void) -> *mut c_void;
    (External(Ptr(Ref))) -> Infallible(External(Ptr(Mut)), false)
  );
  test!(
    fn op_print(#[string] msg: &str, is_err: bool) -> Result<(), Error>;
    (String(RefStr), Numeric(bool, None)) -> Result(Void, false)
  );
  test!(
    #[string] fn op_lots_of_strings(#[string] s: String, #[string] s2: Option<String>, #[string] s3: Cow<str>, #[string(onebyte)] s4: Cow<[u8]>) -> String;
    (String(String), OptionString(String), String(CowStr), String(CowByte)) -> Infallible(String(String), false)
  );
  test!(
    #[string] fn op_lots_of_option_strings(#[string] s: Option<String>, #[string] s2: Option<&str>, #[string] s3: Option<Cow<str>>) -> Option<String>;
    (OptionString(String), OptionString(RefStr), OptionString(CowStr)) -> Infallible(OptionString(String), false)
  );
  test!(
    fn op_scope<'s>(#[string] msg: &'s str);
    <'s> (String(RefStr)) -> Infallible(Void, false)
  );
  test!(
    fn op_scope_and_generics<'s, AB, BC>(#[string] msg: &'s str) where AB: some::Trait, BC: OtherTrait;
    <'s, AB: some::Trait, BC: OtherTrait> (String(RefStr)) -> Infallible(Void, false)
  );
  test!(
    fn op_generics_static<'s, AB, BC>(#[string] msg: &'s str) where AB: some::Trait + 'static, BC: OtherTrait;
    <'s, AB: some::Trait + 'static, BC: OtherTrait> (String(RefStr)) -> Infallible(Void, false)
  );
  test!(
    fn op_v8_types(s: &mut v8::String, sopt: Option<&mut v8::String>, s2: v8::Local<v8::String>, #[global] s3: v8::Global<v8::String>);
    (V8Ref(Mut, String), OptionV8Ref(Mut, String), V8Local(String), V8Global(String)) -> Infallible(Void, false)
  );
  test!(
    fn op_v8_scope<'s>(scope: &mut v8::HandleScope<'s>);
    <'s> (Ref(Mut, HandleScope)) -> Infallible(Void, false)
  );
  test!(
    fn op_state_rc(state: Rc<RefCell<OpState>>);
    (RcRefCell(OpState)) -> Infallible(Void, false)
  );
  test!(
    fn op_state_ref(state: &OpState);
    (Ref(Ref, OpState)) -> Infallible(Void, false)
  );
  test!(
    fn op_state_attr(#[state] something: &Something, #[state] another: Option<&Something>);
    (State(Ref, Something), OptionState(Ref, Something)) -> Infallible(Void, false)
  );
  test!(
    #[buffer] fn op_buffers(#[buffer(copy)] a: Vec<u8>, #[buffer(copy)] b: Box<[u8]>, #[buffer(copy)] c: bytes::Bytes,
      #[buffer] d: V8Slice<u8>, #[buffer] e: JsBuffer, #[buffer(detach)] f: JsBuffer) -> Vec<u8>;
    (Buffer(Vec(u8), Copy, TypedArray), Buffer(BoxSlice(u8), Copy, TypedArray),
      Buffer(Bytes, Copy, TypedArray), Buffer(V8Slice(u8), Default, TypedArray),
      Buffer(JsBuffer, Default, TypedArray), Buffer(JsBuffer, Detach, TypedArray)) -> Infallible(Buffer(Vec(u8), Default, TypedArray), false)
  );
  test!(
    #[buffer] fn op_return_bytesmut() -> bytes::BytesMut;
    () -> Infallible(Buffer(BytesMut, Default, TypedArray), false)
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
  test!(
    fn op_js_runtime_state_ref(state: &JsRuntimeState);
    (Ref(Ref, JsRuntimeState)) -> Infallible(Void, false)
  );
  test!(
    fn op_js_runtime_state_mut(state: &mut JsRuntimeState);
    (Ref(Mut, JsRuntimeState)) -> Infallible(Void, false)
  );
  test!(
    fn op_js_runtime_state_rc(state: Rc<JsRuntimeState>);
    (Rc(JsRuntimeState)) -> Infallible(Void, false)
  );
  test!(
    fn op_isolate(isolate: *mut v8::Isolate);
    (Special(Isolate)) -> Infallible(Void, false)
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
    (RcRefCell(OpState), Numeric(__SMI__, None)) -> FutureResult(SerdeV8(ExtremelyLongTypeNameThatForcesEverythingToWrapAndAddsCommas))
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
    op_with_missing_global,
    ArgError(
      "g".into(),
      MissingAttribute("global", "v8::Global<v8::String>".into())
    ),
    fn f(g: v8::Global<v8::String>) {}
  );
  expect_fail!(
    op_with_invalid_global,
    ArgError(
      "l".into(),
      InvalidAttributeType("global", "v8::Local<v8::String>".into())
    ),
    fn f(#[global] l: v8::Local<v8::String>) {}
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

  expect_fail!(op_with_two_lifetimes, TooManyLifetimes, fn f<'a, 'b>() {});
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

  expect_fail!(
    op_with_bad_from_v8_string,
    ArgError("s".into(), InvalidAttributeType("from_v8", "String".into())),
    fn f(#[from_v8] s: String) {}
  );

  expect_fail!(
    op_with_to_v8_arg,
    ArgError(
      "s".into(),
      InvalidAttributePosition("to_v8", "return value")
    ),
    fn f(#[to_v8] s: Foo) {}
  );

  expect_fail!(
    op_with_from_v8_ret,
    RetError(super::RetError::InvalidType(InvalidAttributePosition(
      "from_v8", "argument"
    ))),
    #[from_v8]
    fn f() -> Foo {}
  );
}
