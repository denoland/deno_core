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
      WebIdlErrorKind::IntNotFinite => write!(f, "is not a finite number"),
      WebIdlErrorKind::IntRange { lower_bound, upper_bound } => write!(f, "is outside the accepted range of ${lower_bound} to ${upper_bound}, inclusive"),
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
  IntNotFinite,
  IntRange {
    lower_bound: f64,
    upper_bound: f64,
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

#[derive(Debug, Default)]
pub struct IntOptions {
  pub clamp: bool,
  pub enforce_range: bool,
}

/*
todo:
If bitLength is 64, then:

    Let upperBound be 2^53 − 1.

    If signedness is "unsigned", then let lowerBound be 0.

    Otherwise let lowerBound be −2^53 + 1.

 */
// https://webidl.spec.whatwg.org/#abstract-opdef-converttoint
macro_rules! impl_ints {
  ($($t:ty: $unsigned:tt = $name: literal ),*) => {
    $(
      impl<'a> WebIdlConverter<'a> for $t {
        type Options = IntOptions;

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
              return Err(WebIdlError::new(prefix, &context, WebIdlErrorKind::IntNotFinite));
            }

            n = n.trunc();
            if n == -0.0 {
              n = 0.0;
            }

            if n < Self::MIN as f64 || n > Self::MAX as f64 {
              return Err(WebIdlError::new(prefix, &context, WebIdlErrorKind::IntRange {
                lower_bound: Self::MIN as f64,
                upper_bound: Self::MAX as f64,
              }));
            }

            return Ok(n as Self);
          }

          if !n.is_nan() && options.clamp {
            return Ok(
              n.clamp(Self::MIN as f64, Self::MAX as f64)
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

          if n >= Self::MIN as f64 && n <= Self::MAX as f64 {
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
    if $n >= Self::MAX as f64 {
      return Ok(($n - $bit_len_num) as Self);
    }
  };

  (@handle_unsigned true $n:ident $bit_len_num:ident) => {};
}

// https://webidl.spec.whatwg.org/#js-integer-types
impl_ints!(
  i8: false = "byte",
  u8: true = "octet",
  i16: false = "short",
  u16: true = "unsigned short",
  i32: false = "long",
  u32: true = "unsigned long",
  i64: false = "long long",
  u64: true = "unsigned long long"
);

// TODO: float, unrestricted float, double, unrestricted double

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
        if eternal.is_empty() {
          let key = NEXT
            .v8_string(scope)
            .map_err(|e| WebIdlError::other(prefix.clone(), &context, e))?;
          eternal.set(scope, key);
          Ok(key)
        } else {
          Ok(eternal.get(scope))
        }
      })?
      .into();

    let done_key = DONE_ETERNAL
      .with(|eternal| {
        if eternal.is_empty() {
          let key = DONE
            .v8_string(scope)
            .map_err(|e| WebIdlError::other(prefix.clone(), &context, e))?;
          eternal.set(scope, key);
          Ok(key)
        } else {
          Ok(eternal.get(scope))
        }
      })?
      .into();

    let value_key = VALUE_ETERNAL
      .with(|eternal| {
        if eternal.is_empty() {
          let key = VALUE
            .v8_string(scope)
            .map_err(|e| WebIdlError::other(prefix.clone(), &context, e))?;
          eternal.set(scope, key);
          Ok(key)
        } else {
          Ok(eternal.get(scope))
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
      todo!()
    }
  }
}

pub struct DOMString(pub String);

#[derive(Debug, Default)]
pub struct DOMStringOptions {
  treat_null_as_empty_string: bool,
}

impl<'a> WebIdlConverter<'a> for DOMString {
  type Options = DOMStringOptions;

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

    Ok(Self(str.to_rust_string_lossy(scope)))
  }
}

pub struct ByteString(pub String);
impl<'a> WebIdlConverter<'a> for ByteString {
  type Options = DOMStringOptions;

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
    let dom_str = DOMString::convert(scope, value, prefix, context, options)?;

    Ok(Self(dom_str.0))
  }
}
