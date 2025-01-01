// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

use std::borrow::Cow;
use std::collections::HashMap;
use v8::HandleScope;
use v8::Local;
use v8::Value;

#[derive(Debug)]
pub struct WebIdlError {
  pub prefix: Cow<'static, str>,
  pub context: Cow<'static, str>,
  pub kind: WebIdlErrorKind,
}

impl WebIdlError {
  pub fn new(
    prefix: Cow<'static, str>,
    context: &impl Fn() -> Cow<'static, str>,
    kind: WebIdlErrorKind,
  ) -> Self {
    Self {
      prefix,
      context: context(),
      kind,
    }
  }

  pub fn other<T: std::error::Error + Send + Sync + 'static>(
    prefix: Cow<'static, str>,
    context: &impl Fn() -> Cow<'static, str>,
    other: T,
  ) -> Self {
    Self::new(prefix, context, WebIdlErrorKind::Other(Box::new(other)))
  }
}

impl std::fmt::Display for WebIdlError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}: {}", self.prefix, self.context)?;

    match &self.kind {
      WebIdlErrorKind::ConvertToConverterType(kind) => {
        write!(f, "can not be converted to a {kind}")
      }
      WebIdlErrorKind::DictionaryCannotConvertKey { converter, key } => {
        write!(
          f,
          "can not be converted to '{converter}' because '{key}' is required in '{converter}'",
        )
      }
      WebIdlErrorKind::NotFinite => write!(f, "is not a finite number"),
      WebIdlErrorKind::IntRange { lower_bound, upper_bound } => write!(f, "is outside the accepted range of ${lower_bound} to ${upper_bound}, inclusive"),
      WebIdlErrorKind::InvalidByteString => write!(f, "is not a valid ByteString"),
      WebIdlErrorKind::Precision => write!(f, "is outside the range of a single-precision floating-point value"),
      WebIdlErrorKind::InvalidEnumVariant { converter, variant } => write!(f, "can not be converted to '{converter}' because '{variant}' is not a valid enum value"),
      WebIdlErrorKind::Other(other) => std::fmt::Display::fmt(other, f),
    }
  }
}

impl std::error::Error for WebIdlError {}

#[derive(Debug)]
pub enum WebIdlErrorKind {
  ConvertToConverterType(&'static str),
  DictionaryCannotConvertKey {
    converter: &'static str,
    key: &'static str,
  },
  NotFinite,
  IntRange {
    lower_bound: f64,
    upper_bound: f64,
  },
  Precision,
  InvalidByteString,
  InvalidEnumVariant {
    converter: &'static str,
    variant: String,
  },
  Other(Box<dyn std::error::Error + Send + Sync + 'static>),
}

pub type WebIdlContext = Box<dyn Fn() -> Cow<'static, str>>;

pub trait WebIdlConverter<'a>: Sized {
  type Options: Default;

  fn convert<C>(
    scope: &mut HandleScope<'a>,
    value: Local<'a, Value>,
    prefix: Cow<'static, str>,
    context: C,
    options: &Self::Options,
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>;
}

// any converter
impl<'a> WebIdlConverter<'a> for Local<'a, Value> {
  type Options = ();

  fn convert<C>(
    _scope: &mut HandleScope<'a>,
    value: Local<'a, Value>,
    _prefix: Cow<'static, str>,
    _context: C,
    _options: &Self::Options,
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    Ok(value)
  }
}

// nullable converter
impl<'a, T: WebIdlConverter<'a>> WebIdlConverter<'a> for Option<T> {
  type Options = T::Options;

  fn convert<C>(
    scope: &mut HandleScope<'a>,
    value: Local<'a, Value>,
    prefix: Cow<'static, str>,
    context: C,
    options: &Self::Options,
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    if value.is_null_or_undefined() {
      Ok(None)
    } else {
      Ok(Some(WebIdlConverter::convert(
        scope, value, prefix, context, options,
      )?))
    }
  }
}

crate::v8_static_strings! {
  NEXT = "next",
  DONE = "done",
  VALUE = "value",
}

thread_local! {
  static NEXT_ETERNAL: v8::Eternal<v8::String> = v8::Eternal::empty();
  static DONE_ETERNAL: v8::Eternal<v8::String> = v8::Eternal::empty();
  static VALUE_ETERNAL: v8::Eternal<v8::String> = v8::Eternal::empty();
}

// sequence converter
impl<'a, T: WebIdlConverter<'a>> WebIdlConverter<'a> for Vec<T> {
  type Options = T::Options;

  fn convert<C>(
    scope: &mut HandleScope<'a>,
    value: Local<'a, Value>,
    prefix: Cow<'static, str>,
    context: C,
    options: &Self::Options,
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    let Some(obj) = value.to_object(scope) else {
      return Err(WebIdlError::new(
        prefix,
        &context,
        WebIdlErrorKind::ConvertToConverterType("sequence"),
      ));
    };

    let iter_key = v8::Symbol::get_iterator(scope);
    let Some(iter) = obj
      .get(scope, iter_key.into())
      .and_then(|iter| iter.try_cast::<v8::Function>().ok())
      .and_then(|iter| iter.call(scope, obj.cast(), &[]))
      .and_then(|iter| iter.to_object(scope))
    else {
      return Err(WebIdlError::new(
        prefix,
        &context,
        WebIdlErrorKind::ConvertToConverterType("sequence"),
      ));
    };

    let mut out = vec![];

    let next_key = NEXT_ETERNAL
      .with(|eternal| {
        if let Some(key) = eternal.get(scope) {
          Ok(key)
        } else {
          let key = NEXT
            .v8_string(scope)
            .map_err(|e| WebIdlError::other(prefix.clone(), &context, e))?;
          eternal.set(scope, key);
          Ok(key)
        }
      })?
      .into();

    let done_key = DONE_ETERNAL
      .with(|eternal| {
        if let Some(key) = eternal.get(scope) {
          Ok(key)
        } else {
          let key = DONE
            .v8_string(scope)
            .map_err(|e| WebIdlError::other(prefix.clone(), &context, e))?;
          eternal.set(scope, key);
          Ok(key)
        }
      })?
      .into();

    let value_key = VALUE_ETERNAL
      .with(|eternal| {
        if let Some(key) = eternal.get(scope) {
          Ok(key)
        } else {
          let key = VALUE
            .v8_string(scope)
            .map_err(|e| WebIdlError::other(prefix.clone(), &context, e))?;
          eternal.set(scope, key);
          Ok(key)
        }
      })?
      .into();

    loop {
      let Some(res) = iter
        .get(scope, next_key)
        .and_then(|next| next.try_cast::<v8::Function>().ok())
        .and_then(|next| next.call(scope, iter.cast(), &[]))
        .and_then(|res| res.to_object(scope))
      else {
        return Err(WebIdlError::new(
          prefix,
          &context,
          WebIdlErrorKind::ConvertToConverterType("sequence"),
        ));
      };

      if res.get(scope, done_key).is_some_and(|val| val.is_true()) {
        break;
      }

      let Some(iter_val) = res.get(scope, value_key) else {
        return Err(WebIdlError::new(
          prefix,
          &context,
          WebIdlErrorKind::ConvertToConverterType("sequence"),
        ));
      };

      out.push(WebIdlConverter::convert(
        scope,
        iter_val,
        prefix.clone(),
        || format!("{}, index {}", context(), out.len()).into(),
        options,
      )?);
    }

    Ok(out)
  }
}

// record converter
// the Options only apply to the value, not the key
impl<
    'a,
    K: WebIdlConverter<'a> + Eq + std::hash::Hash,
    V: WebIdlConverter<'a>,
  > WebIdlConverter<'a> for HashMap<K, V>
{
  type Options = V::Options;

  fn convert<C>(
    scope: &mut HandleScope<'a>,
    value: Local<'a, Value>,
    prefix: Cow<'static, str>,
    context: C,
    options: &Self::Options,
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    let Ok(obj) = value.try_cast::<v8::Object>() else {
      return Err(WebIdlError::new(
        prefix,
        &context,
        WebIdlErrorKind::ConvertToConverterType("record"),
      ));
    };

    if !obj.is_proxy() {
      let Some(keys) = obj.get_own_property_names(
        scope,
        v8::GetPropertyNamesArgs {
          mode: v8::KeyCollectionMode::OwnOnly,
          property_filter: Default::default(),
          index_filter: v8::IndexFilter::IncludeIndices,
          key_conversion: v8::KeyConversionMode::ConvertToString,
        },
      ) else {
        return Ok(Default::default());
      };

      let mut out = HashMap::with_capacity(keys.length() as _);

      for i in 0..keys.length() {
        let key = keys.get_index(scope, i).unwrap();
        let value = obj.get(scope, key).unwrap();

        let key = WebIdlConverter::convert(
          scope,
          key,
          prefix.clone(),
          &context,
          &Default::default(),
        )?;
        let value = WebIdlConverter::convert(
          scope,
          value,
          prefix.clone(),
          &context,
          options,
        )?;

        out.insert(key, value);
      }

      Ok(out)
    } else {
      todo!("handle proxy")
    }
  }
}

#[derive(Debug, Default)]
pub struct IntOptions {
  pub clamp: bool,
  pub enforce_range: bool,
}

// https://webidl.spec.whatwg.org/#abstract-opdef-converttoint
macro_rules! impl_ints {
  ($($t:ty: $unsigned:tt = $name:literal: $min:expr => $max:expr),*) => {
    $(
      impl<'a> WebIdlConverter<'a> for $t {
        type Options = IntOptions;

        #[allow(clippy::manual_range_contains)]
        fn convert<C>(
          scope: &mut HandleScope<'a>,
          value: Local<'a, Value>,
          prefix: Cow<'static, str>,
          context: C,
          options: &Self::Options,
        ) -> Result<Self, WebIdlError>
        where
          C: Fn() -> Cow<'static, str>,
        {
          const MIN: f64 = $min as f64;
          const MAX: f64 = $max as f64;

          if value.is_big_int() {
            return Err(WebIdlError::new(prefix, &context, WebIdlErrorKind::ConvertToConverterType($name)));
          }

          let Some(mut n) = value.number_value(scope) else {
            return Err(WebIdlError::new(prefix, &context, WebIdlErrorKind::ConvertToConverterType($name)));
          };
          if n == -0.0 {
            n = 0.0;
          }

          if options.enforce_range {
            if !n.is_finite() {
              return Err(WebIdlError::new(prefix, &context, WebIdlErrorKind::NotFinite));
            }

            n = n.trunc();
            if n == -0.0 {
              n = 0.0;
            }

            if n < MIN || n > MAX {
              return Err(WebIdlError::new(prefix, &context, WebIdlErrorKind::IntRange {
                lower_bound: MIN,
                upper_bound: MAX,
              }));
            }

            return Ok(n as Self);
          }

          if !n.is_nan() && options.clamp {
            return Ok(
              n.clamp(MIN, MAX)
              .round_ties_even() as Self
            );
          }

          if !n.is_finite() || n == 0.0 {
            return Ok(0);
          }

          n = n.trunc();
          if n == -0.0 {
            n = 0.0;
          }

          if n >= MIN && n <= MAX {
            return Ok(n as Self);
          }

          let bit_len_num = 2.0f64.powi(Self::BITS as i32);

          n = {
            let sign_might_not_match = n % bit_len_num;
            if n.is_sign_positive() != bit_len_num.is_sign_positive() {
              sign_might_not_match + bit_len_num
            } else {
              sign_might_not_match
            }
          };

          impl_ints!(@handle_unsigned $unsigned n bit_len_num);

          Ok(n as Self)
        }
      }
    )*
  };

  (@handle_unsigned false $n:ident $bit_len_num:ident) => {
    if $n >= MAX {
      return Ok(($n - $bit_len_num) as Self);
    }
  };

  (@handle_unsigned true $n:ident $bit_len_num:ident) => {};
}

// https://webidl.spec.whatwg.org/#js-integer-types
impl_ints!(
  i8:  false = "byte":               i8::MIN => i8::MAX,
  u8:  true  = "octet":              u8::MIN => u8::MAX,
  i16: false = "short":              i16::MIN => i16::MAX,
  u16: true  = "unsigned short":     u16::MIN => u16::MAX,
  i32: false = "long":               i32::MIN => i32::MAX,
  u32: true  = "unsigned long":      u32::MIN => u32::MAX,
  i64: false = "long long":          ((-2i64).pow(53) + 1) => (2i64.pow(53) - 1),
  u64: true  = "unsigned long long": u64::MIN => (2u64.pow(53) - 1)
);

// float
impl<'a> WebIdlConverter<'a> for f32 {
  type Options = ();

  fn convert<C>(
    scope: &mut HandleScope<'a>,
    value: Local<'a, Value>,
    prefix: Cow<'static, str>,
    context: C,
    _options: &Self::Options,
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    let Some(n) = value.number_value(scope) else {
      return Err(WebIdlError::new(
        prefix,
        &context,
        WebIdlErrorKind::ConvertToConverterType("float"),
      ));
    };

    if !n.is_finite() {
      return Err(WebIdlError::new(
        prefix,
        &context,
        WebIdlErrorKind::NotFinite,
      ));
    }

    let n = n as f32;

    if !n.is_finite() {
      return Err(WebIdlError::new(
        prefix,
        &context,
        WebIdlErrorKind::Precision,
      ));
    }

    Ok(n)
  }
}

pub struct UnrestrictedFloat(pub f32);
impl std::ops::Deref for UnrestrictedFloat {
  type Target = f32;
  fn deref(&self) -> &Self::Target {
    &self.0
  }
}

impl<'a> WebIdlConverter<'a> for UnrestrictedFloat {
  type Options = ();

  fn convert<C>(
    scope: &mut HandleScope<'a>,
    value: Local<'a, Value>,
    prefix: Cow<'static, str>,
    context: C,
    _options: &Self::Options,
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    let Some(n) = value.number_value(scope) else {
      return Err(WebIdlError::new(
        prefix,
        &context,
        WebIdlErrorKind::ConvertToConverterType("float"),
      ));
    };

    Ok(UnrestrictedFloat(n as f32))
  }
}

// double
impl<'a> WebIdlConverter<'a> for f64 {
  type Options = ();

  fn convert<C>(
    scope: &mut HandleScope<'a>,
    value: Local<'a, Value>,
    prefix: Cow<'static, str>,
    context: C,
    _options: &Self::Options,
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    let Some(n) = value.number_value(scope) else {
      return Err(WebIdlError::new(
        prefix,
        &context,
        WebIdlErrorKind::ConvertToConverterType("float"),
      ));
    };

    if !n.is_finite() {
      return Err(WebIdlError::new(
        prefix,
        &context,
        WebIdlErrorKind::NotFinite,
      ));
    }

    Ok(n)
  }
}

pub struct UnrestrictedDouble(pub f64);
impl std::ops::Deref for UnrestrictedDouble {
  type Target = f64;
  fn deref(&self) -> &Self::Target {
    &self.0
  }
}

impl<'a> WebIdlConverter<'a> for UnrestrictedDouble {
  type Options = ();

  fn convert<C>(
    scope: &mut HandleScope<'a>,
    value: Local<'a, Value>,
    prefix: Cow<'static, str>,
    context: C,
    _options: &Self::Options,
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    let Some(n) = value.number_value(scope) else {
      return Err(WebIdlError::new(
        prefix,
        &context,
        WebIdlErrorKind::ConvertToConverterType("float"),
      ));
    };

    Ok(UnrestrictedDouble(n))
  }
}

#[derive(Debug)]
pub struct BigInt {
  pub sign: bool,
  pub words: Vec<u64>,
}

impl<'a> WebIdlConverter<'a> for BigInt {
  type Options = ();

  fn convert<C>(
    scope: &mut HandleScope<'a>,
    value: Local<'a, Value>,
    prefix: Cow<'static, str>,
    context: C,
    _options: &Self::Options,
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    let Some(bigint) = value.to_big_int(scope) else {
      return Err(WebIdlError::new(
        prefix,
        &context,
        WebIdlErrorKind::ConvertToConverterType("bigint"),
      ));
    };

    let mut words = vec![];
    let (sign, _) = bigint.to_words_array(&mut words);
    Ok(Self { sign, words })
  }
}

impl<'a> WebIdlConverter<'a> for bool {
  type Options = ();

  fn convert<C>(
    scope: &mut HandleScope<'a>,
    value: Local<'a, Value>,
    _prefix: Cow<'static, str>,
    _context: C,
    _options: &Self::Options,
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    Ok(value.to_boolean(scope).is_true())
  }
}

#[derive(Debug, Default)]
pub struct StringOptions {
  treat_null_as_empty_string: bool,
}

// DOMString and USVString, since we treat them the same
impl<'a> WebIdlConverter<'a> for String {
  type Options = StringOptions;

  fn convert<C>(
    scope: &mut HandleScope<'a>,
    value: Local<'a, Value>,
    prefix: Cow<'static, str>,
    context: C,
    options: &Self::Options,
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    let str = if value.is_string() {
      value.try_cast::<v8::String>().unwrap()
    } else if value.is_null() && options.treat_null_as_empty_string {
      return Ok(String::new());
    } else if value.is_symbol() {
      return Err(WebIdlError::new(
        prefix,
        &context,
        WebIdlErrorKind::ConvertToConverterType("string"),
      ));
    } else if let Some(str) = value.to_string(scope) {
      str
    } else {
      return Err(WebIdlError::new(
        prefix,
        &context,
        WebIdlErrorKind::ConvertToConverterType("string"),
      ));
    };

    Ok(str.to_rust_string_lossy(scope))
  }
}

pub struct ByteString(pub String);
impl std::ops::Deref for ByteString {
  type Target = String;
  fn deref(&self) -> &Self::Target {
    &self.0
  }
}
impl<'a> WebIdlConverter<'a> for ByteString {
  type Options = StringOptions;

  fn convert<C>(
    scope: &mut HandleScope<'a>,
    value: Local<'a, Value>,
    prefix: Cow<'static, str>,
    context: C,
    options: &Self::Options,
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    let str = if value.is_string() {
      value.try_cast::<v8::String>().unwrap()
    } else if value.is_null() && options.treat_null_as_empty_string {
      return Ok(Self(String::new()));
    } else if value.is_symbol() {
      return Err(WebIdlError::new(
        prefix,
        &context,
        WebIdlErrorKind::ConvertToConverterType("string"),
      ));
    } else if let Some(str) = value.to_string(scope) {
      str
    } else {
      return Err(WebIdlError::new(
        prefix,
        &context,
        WebIdlErrorKind::ConvertToConverterType("string"),
      ));
    };

    if !str.contains_only_onebyte() {
      return Err(WebIdlError::new(
        prefix,
        &context,
        WebIdlErrorKind::InvalidByteString,
      ));
    }

    Ok(Self(str.to_rust_string_lossy(scope)))
  }
}

// TODO:
//  object
//  ArrayBuffer
//  DataView
//  Array buffer types
//  ArrayBufferView
