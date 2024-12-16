use std::borrow::Cow;
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
    context: Cow<'static, str>,
    kind: WebIdlErrorKind,
  ) -> Self {
    Self {
      prefix,
      context,
      kind,
    }
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
  Other(Box<dyn std::error::Error + Send + Sync + 'static>),
}

pub trait WebIdlConverter<'a>: Sized {
  fn convert(
    scope: &mut HandleScope<'a>,
    value: Local<'a, Value>,
    prefix: Cow<'static, str>,
    context: Cow<'static, str>,
  ) -> Result<Self, WebIdlError>;
}

impl<'a, T: crate::FromV8<'a>> WebIdlConverter<'a> for T {
  fn convert(
    scope: &mut HandleScope<'a>,
    value: Local<'a, Value>,
    prefix: Cow<'static, str>,
    context: Cow<'static, str>,
  ) -> Result<Self, WebIdlError> {
    Self::from_v8(scope, value).map_err(|e| {
      WebIdlError::new(prefix, context, WebIdlErrorKind::Other(Box::new(e)))
    })
  }
}

impl<'a, T: WebIdlConverter<'a>> WebIdlConverter<'a> for Vec<T> {
  fn convert(
    scope: &mut HandleScope<'a>,
    value: Local<'a, Value>,
    prefix: Cow<'static, str>,
    context: Cow<'static, str>,
  ) -> Result<Self, WebIdlError> {
    let Some(obj) = value.to_object(scope) else {
      return Err(WebIdlError::new(
        prefix,
        context,
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
        context,
        WebIdlErrorKind::ConvertToConverterType("sequence"),
      ));
    };

    let mut out = vec![];

    let next_key = v8::String::new(scope, "next").unwrap();
    let done_key = v8::String::new(scope, "done").unwrap();
    let value_key = v8::String::new(scope, "value").unwrap();

    loop {
      let Some(res) = iter
        .get(scope, next_key.into())
        .and_then(|next| next.try_cast::<v8::Function>().ok())
        .and_then(|next| next.call(scope, iter.cast(), &[]))
        .and_then(|res| res.to_object(scope))
      else {
        return Err(WebIdlError::new(
          prefix,
          context,
          WebIdlErrorKind::ConvertToConverterType("sequence"),
        ));
      };

      if res
        .get(scope, done_key.into())
        .is_some_and(|val| val.is_true())
      {
        break;
      }

      let Some(iter_val) = res.get(scope, value_key.into()) else {
        return Err(WebIdlError::new(
          prefix,
          context,
          WebIdlErrorKind::ConvertToConverterType("sequence"),
        ));
      };

      out.push(WebIdlConverter::convert(
        scope,
        iter_val,
        prefix.clone(),
        format!("{context}, index {}", out.len()).into(),
      )?);
    }

    Ok(out)
  }
}
