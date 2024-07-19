// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

use std::borrow::Cow;
use std::collections::HashSet;
use std::fmt;
use std::fmt::Debug;
use std::fmt::Display;
use std::fmt::Formatter;

use anyhow::Error;

use crate::runtime::v8_static_strings;
use crate::runtime::JsRealm;
use crate::runtime::JsRuntime;
use crate::source_map::SourceMapApplication;
use crate::url::Url;
use crate::FastStaticString;

/// A generic wrapper that can encapsulate any concrete error type.
// TODO(ry) Deprecate AnyError and encourage deno_core::anyhow::Error instead.
pub type AnyError = anyhow::Error;

pub type JsErrorCreateFn = dyn Fn(JsError) -> Error;
pub type GetErrorClassFn = &'static dyn for<'e> Fn(&'e Error) -> &'static str;

/// Creates a new error with a caller-specified error class name and message.
pub fn custom_error(
  class: &'static str,
  message: impl Into<Cow<'static, str>>,
) -> Error {
  CustomError {
    class,
    message: message.into(),
  }
  .into()
}

pub fn generic_error(message: impl Into<Cow<'static, str>>) -> Error {
  custom_error("Error", message)
}

pub fn type_error(message: impl Into<Cow<'static, str>>) -> Error {
  custom_error("TypeError", message)
}

pub fn range_error(message: impl Into<Cow<'static, str>>) -> Error {
  custom_error("RangeError", message)
}

pub fn invalid_hostname(hostname: &str) -> Error {
  type_error(format!("Invalid hostname: '{hostname}'"))
}

pub fn uri_error(message: impl Into<Cow<'static, str>>) -> Error {
  custom_error("URIError", message)
}

pub fn bad_resource(message: impl Into<Cow<'static, str>>) -> Error {
  custom_error("BadResource", message)
}

pub fn bad_resource_id() -> Error {
  custom_error("BadResource", "Bad resource ID")
}

pub fn not_supported() -> Error {
  custom_error("NotSupported", "The operation is not supported")
}

pub fn resource_unavailable() -> Error {
  custom_error(
    "Busy",
    "Resource is unavailable because it is in use by a promise",
  )
}

/// A simple error type that lets the creator specify both the error message and
/// the error class name. This type is private; externally it only ever appears
/// wrapped in an `anyhow::Error`. To retrieve the error class name from a wrapped
/// `CustomError`, use the function `get_custom_error_class()`.
#[derive(Debug)]
struct CustomError {
  class: &'static str,
  message: Cow<'static, str>,
}

impl Display for CustomError {
  fn fmt(&self, f: &mut Formatter) -> fmt::Result {
    f.write_str(&self.message)
  }
}

impl std::error::Error for CustomError {}

/// If this error was crated with `custom_error()`, return the specified error
/// class name. In all other cases this function returns `None`.
pub fn get_custom_error_class(error: &Error) -> Option<&'static str> {
  error.downcast_ref::<CustomError>().map(|e| e.class)
}

/// A wrapper around `anyhow::Error` that implements `std::error::Error`
#[repr(transparent)]
pub struct StdAnyError(pub Error);
impl std::fmt::Debug for StdAnyError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(f, "{:?}", self.0)
  }
}

impl std::fmt::Display for StdAnyError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", self.0)
  }
}

impl std::error::Error for StdAnyError {
  fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
    self.0.source()
  }
}

impl From<Error> for StdAnyError {
  fn from(err: Error) -> Self {
    Self(err)
  }
}

pub fn to_v8_error<'a>(
  scope: &mut v8::HandleScope<'a>,
  get_class: GetErrorClassFn,
  error: &Error,
) -> v8::Local<'a, v8::Value> {
  let tc_scope = &mut v8::TryCatch::new(scope);
  let cb = JsRealm::exception_state_from_scope(tc_scope)
    .js_build_custom_error_cb
    .borrow()
    .clone()
    .expect("Custom error builder must be set");
  let cb = cb.open(tc_scope);
  let this = v8::undefined(tc_scope).into();
  let class = v8::String::new(tc_scope, get_class(error)).unwrap();
  let message = v8::String::new(tc_scope, &format!("{error:#}")).unwrap();
  let mut args = vec![class.into(), message.into()];
  if let Some(code) = crate::error_codes::get_error_code(error) {
    args.push(v8::String::new(tc_scope, code).unwrap().into());
  }
  let maybe_exception = cb.call(tc_scope, this, &args);

  match maybe_exception {
    Some(exception) => exception,
    None => {
      let mut msg =
        "Custom error class must have a builder registered".to_string();
      if tc_scope.has_caught() {
        let e = tc_scope.exception().unwrap();
        let js_error = JsError::from_v8_exception(tc_scope, e);
        msg = format!("{}: {}", msg, js_error.exception_message);
      }
      panic!("{}", msg);
    }
  }
}

#[inline(always)]
pub(crate) fn call_site_evals_key<'a>(
  scope: &mut v8::HandleScope<'a>,
) -> v8::Local<'a, v8::Private> {
  let name = v8_static_strings::CALL_SITE_EVALS.v8_string(scope);
  v8::Private::for_api(scope, Some(name))
}

/// A `JsError` represents an exception coming from V8, with stack frames and
/// line numbers. The deno_cli crate defines another `JsError` type, which wraps
/// the one defined here, that adds source map support and colorful formatting.
/// When updating this struct, also update errors_are_equal_without_cause() in
/// fmt_error.rs.
#[derive(Debug, PartialEq, Clone, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JsError {
  pub name: Option<String>,
  pub message: Option<String>,
  pub stack: Option<String>,
  pub cause: Option<Box<JsError>>,
  pub exception_message: String,
  pub frames: Vec<JsStackFrame>,
  pub source_line: Option<String>,
  pub source_line_frame_index: Option<usize>,
  pub aggregated: Option<Vec<JsError>>,
}

#[derive(Debug, Eq, PartialEq, Clone, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JsStackFrame {
  pub type_name: Option<String>,
  pub function_name: Option<String>,
  pub method_name: Option<String>,
  pub file_name: Option<String>,
  pub line_number: Option<i64>,
  pub column_number: Option<i64>,
  pub eval_origin: Option<String>,
  // Warning! isToplevel has inconsistent snake<>camel case, "typo" originates in v8:
  // https://source.chromium.org/search?q=isToplevel&sq=&ss=chromium%2Fchromium%2Fsrc:v8%2F
  #[serde(rename = "isToplevel")]
  pub is_top_level: Option<bool>,
  pub is_eval: bool,
  pub is_native: bool,
  pub is_constructor: bool,
  pub is_async: bool,
  pub is_promise_all: bool,
  pub promise_index: Option<i64>,
}

impl JsStackFrame {
  pub fn from_location(
    file_name: Option<String>,
    line_number: Option<i64>,
    column_number: Option<i64>,
  ) -> Self {
    Self {
      type_name: None,
      function_name: None,
      method_name: None,
      file_name,
      line_number,
      column_number,
      eval_origin: None,
      is_top_level: None,
      is_eval: false,
      is_native: false,
      is_constructor: false,
      is_async: false,
      is_promise_all: false,
      promise_index: None,
    }
  }

  /// Gets the source mapped stack frame corresponding to the
  /// (script_resource_name, line_number, column_number) from a v8 message.
  /// For non-syntax errors, it should also correspond to the first stack frame.
  pub fn from_v8_message<'a>(
    scope: &'a mut v8::HandleScope,
    message: v8::Local<'a, v8::Message>,
  ) -> Option<Self> {
    let f = message.get_script_resource_name(scope)?;
    let f: v8::Local<v8::String> = f.try_into().ok()?;
    let f = f.to_rust_string_lossy(scope);
    let l = message.get_line_number(scope)? as u32;
    // V8's column numbers are 0-based, we want 1-based.
    let c = message.get_start_column() as u32 + 1;
    let state = JsRuntime::state_from(scope);
    let mut source_mapper = state.source_mapper.borrow_mut();
    match source_mapper.apply_source_map(&f, l, c) {
      SourceMapApplication::Unchanged => Some(JsStackFrame::from_location(
        Some(f),
        Some(l.into()),
        Some(c.into()),
      )),
      SourceMapApplication::LineAndColumn {
        line_number,
        column_number,
      } => Some(JsStackFrame::from_location(
        Some(f),
        Some(line_number.into()),
        Some(column_number.into()),
      )),
      SourceMapApplication::LineAndColumnAndFileName {
        file_name,
        line_number,
        column_number,
      } => Some(JsStackFrame::from_location(
        Some(file_name),
        Some(line_number.into()),
        Some(column_number.into()),
      )),
    }
  }

  pub fn maybe_format_location(&self) -> Option<String> {
    Some(format!(
      "{}:{}:{}",
      self.file_name.as_ref()?,
      self.line_number?,
      self.column_number?
    ))
  }
}

#[inline(always)]
fn get_property<'a>(
  scope: &mut v8::HandleScope<'a>,
  object: v8::Local<v8::Object>,
  key: FastStaticString,
) -> Option<v8::Local<'a, v8::Value>> {
  let key = key.v8_string(scope);
  object.get(scope, key.into())
}

#[derive(Default, serde::Deserialize)]
pub(crate) struct NativeJsError {
  pub name: Option<String>,
  pub message: Option<String>,
  // Warning! .stack is special so handled by itself
  // stack: Option<String>,
}

impl JsError {
  /// Compares all properties of JsError, except for JsError::cause. This function is used to
  /// detect that 2 JsError objects in a JsError::cause chain are identical, ie. there is a recursive cause.
  ///
  /// We don't have access to object identity here, so we do it via field comparison. Ideally this should
  /// be able to maintain object identity somehow.
  pub fn is_same_error(&self, other: &JsError) -> bool {
    let a = self;
    let b = other;
    // `a.cause == b.cause` omitted, because it is absent in recursive errors,
    // despite the error being identical to a previously seen one.
    a.name == b.name
      && a.message == b.message
      && a.stack == b.stack
      // TODO(mmastrac): we need consistency around when we insert "in promise" and when we don't. For now, we
      // are going to manually replace this part of the string.
      && (a.exception_message == b.exception_message
        || a.exception_message.replace(" (in promise) ", " ") == b.exception_message.replace(" (in promise) ", " "))
      && a.frames == b.frames
      && a.source_line == b.source_line
      && a.source_line_frame_index == b.source_line_frame_index
      && a.aggregated == b.aggregated
  }

  pub fn from_v8_exception(
    scope: &mut v8::HandleScope,
    exception: v8::Local<v8::Value>,
  ) -> Self {
    Self::inner_from_v8_exception(scope, exception, Default::default())
  }

  pub fn from_v8_message<'a>(
    scope: &'a mut v8::HandleScope,
    msg: v8::Local<'a, v8::Message>,
  ) -> Self {
    // Create a new HandleScope because we're creating a lot of new local
    // handles below.
    let scope = &mut v8::HandleScope::new(scope);

    let exception_message = msg.get(scope).to_rust_string_lossy(scope);

    // Convert them into Vec<JsStackFrame>
    let mut frames: Vec<JsStackFrame> = vec![];
    let mut source_line = None;
    let mut source_line_frame_index = None;

    if let Some(stack_frame) = JsStackFrame::from_v8_message(scope, msg) {
      frames = vec![stack_frame];
    }
    {
      let state = JsRuntime::state_from(scope);
      let mut source_mapper = state.source_mapper.borrow_mut();
      for (i, frame) in frames.iter().enumerate() {
        if let (Some(file_name), Some(line_number)) =
          (&frame.file_name, frame.line_number)
        {
          if !file_name.trim_start_matches('[').starts_with("ext:") {
            source_line = source_mapper.get_source_line(file_name, line_number);
            source_line_frame_index = Some(i);
            break;
          }
        }
      }
    }

    Self {
      name: None,
      message: None,
      exception_message,
      cause: None,
      source_line,
      source_line_frame_index,
      frames,
      stack: None,
      aggregated: None,
    }
  }

  fn inner_from_v8_exception<'a>(
    scope: &'a mut v8::HandleScope,
    exception: v8::Local<'a, v8::Value>,
    mut seen: HashSet<v8::Local<'a, v8::Object>>,
  ) -> Self {
    // Create a new HandleScope because we're creating a lot of new local
    // handles below.
    let scope = &mut v8::HandleScope::new(scope);

    let msg = v8::Exception::create_message(scope, exception);

    let mut exception_message = None;
    let exception_state = JsRealm::exception_state_from_scope(scope);

    let js_format_exception_cb =
      exception_state.js_format_exception_cb.borrow().clone();
    if let Some(format_exception_cb) = js_format_exception_cb {
      let format_exception_cb = format_exception_cb.open(scope);
      let this = v8::undefined(scope).into();
      let formatted = format_exception_cb.call(scope, this, &[exception]);
      if let Some(formatted) = formatted {
        if formatted.is_string() {
          exception_message = Some(formatted.to_rust_string_lossy(scope));
        }
      }
    }

    if is_instance_of_error(scope, exception) {
      let v8_exception = exception;
      // The exception is a JS Error object.
      let exception: v8::Local<v8::Object> = exception.try_into().unwrap();
      let cause = get_property(scope, exception, v8_static_strings::CAUSE);
      let e: NativeJsError =
        serde_v8::from_v8(scope, exception.into()).unwrap_or_default();
      // Get the message by formatting error.name and error.message.
      let name = e.name.clone().unwrap_or_else(|| "Error".to_string());
      let message_prop = e.message.clone().unwrap_or_default();
      let exception_message = exception_message.unwrap_or_else(|| {
        if !name.is_empty() && !message_prop.is_empty() {
          format!("Uncaught {name}: {message_prop}")
        } else if !name.is_empty() {
          format!("Uncaught {name}")
        } else if !message_prop.is_empty() {
          format!("Uncaught {message_prop}")
        } else {
          "Uncaught".to_string()
        }
      });
      let cause = cause.and_then(|cause| {
        if cause.is_undefined() || seen.contains(&exception) {
          None
        } else {
          seen.insert(exception);
          Some(Box::new(JsError::inner_from_v8_exception(
            scope, cause, seen,
          )))
        }
      });

      // Access error.stack to ensure that prepareStackTrace() has been called.
      // This should populate error.#callSiteEvals.
      let stack = get_property(scope, exception, v8_static_strings::STACK);
      let stack: Option<v8::Local<v8::String>> =
        stack.and_then(|s| s.try_into().ok());
      let stack = stack.map(|s| s.to_rust_string_lossy(scope));

      // Read an array of structured frames from error.#callSiteEvals.
      let frames_v8 = {
        let key = call_site_evals_key(scope);
        exception.get_private(scope, key)
      };
      // Ignore non-array values
      let frames_v8: Option<v8::Local<v8::Array>> =
        frames_v8.and_then(|a| a.try_into().ok());

      // Convert them into Vec<JsStackFrame>
      let mut frames: Vec<JsStackFrame> = match frames_v8 {
        Some(frames_v8) => serde_v8::from_v8(scope, frames_v8.into()).unwrap(),
        None => vec![],
      };
      let mut source_line = None;
      let mut source_line_frame_index = None;

      // When the stack frame array is empty, but the source location given by
      // (script_resource_name, line_number, start_column + 1) exists, this is
      // likely a syntax error. For the sake of formatting we treat it like it
      // was given as a single stack frame.
      if frames.is_empty() {
        if let Some(stack_frame) = JsStackFrame::from_v8_message(scope, msg) {
          frames = vec![stack_frame];
        }
      }
      {
        let state = JsRuntime::state_from(scope);
        let mut source_mapper = state.source_mapper.borrow_mut();
        for (i, frame) in frames.iter().enumerate() {
          if let (Some(file_name), Some(line_number)) =
            (&frame.file_name, frame.line_number)
          {
            if !file_name.trim_start_matches('[').starts_with("ext:") {
              source_line =
                source_mapper.get_source_line(file_name, line_number);
              source_line_frame_index = Some(i);
              break;
            }
          }
        }
      }

      let mut aggregated: Option<Vec<JsError>> = None;
      if is_aggregate_error(scope, v8_exception) {
        // Read an array of stored errors, this is only defined for `AggregateError`
        let aggregated_errors =
          get_property(scope, exception, v8_static_strings::ERRORS);
        let aggregated_errors: Option<v8::Local<v8::Array>> =
          aggregated_errors.and_then(|a| a.try_into().ok());

        if let Some(errors) = aggregated_errors {
          if errors.length() > 0 {
            let mut agg = vec![];
            for i in 0..errors.length() {
              let error = errors.get_index(scope, i).unwrap();
              let js_error = Self::from_v8_exception(scope, error);
              agg.push(js_error);
            }
            aggregated = Some(agg);
          }
        }
      };

      Self {
        name: e.name,
        message: e.message,
        exception_message,
        cause,
        source_line,
        source_line_frame_index,
        frames,
        stack,
        aggregated,
      }
    } else {
      let exception_message = exception_message
        .unwrap_or_else(|| msg.get(scope).to_rust_string_lossy(scope));
      // The exception is not a JS Error object.
      // Get the message given by V8::Exception::create_message(), and provide
      // empty frames.
      Self {
        name: None,
        message: None,
        exception_message,
        cause: None,
        source_line: None,
        source_line_frame_index: None,
        frames: vec![],
        stack: None,
        aggregated: None,
      }
    }
  }
}

impl std::error::Error for JsError {}

impl Display for JsError {
  fn fmt(&self, f: &mut Formatter) -> fmt::Result {
    if let Some(stack) = &self.stack {
      let stack_lines = stack.lines();
      if stack_lines.count() > 1 {
        return write!(f, "{stack}");
      }
    }
    write!(f, "{}", self.exception_message)?;
    let location = self.frames.first().and_then(|f| f.maybe_format_location());
    if let Some(location) = location {
      write!(f, "\n    at {location}")?;
    }
    Ok(())
  }
}

// TODO(piscisaureus): rusty_v8 should implement the Error trait on
// values of type v8::Global<T>.
pub(crate) fn to_v8_type_error(
  scope: &mut v8::HandleScope,
  err: Error,
) -> v8::Global<v8::Value> {
  let err_string = err.to_string();
  let error_chain = err
    .chain()
    .skip(1)
    .filter(|e| e.to_string() != err_string)
    .map(|e| e.to_string())
    .collect::<Vec<_>>();

  let message = if !error_chain.is_empty() {
    format!(
      "{}\n  Caused by:\n    {}",
      err_string,
      error_chain.join("\n    ")
    )
  } else {
    err_string
  };

  let message = v8::String::new(scope, &message).unwrap();
  let exception = v8::Exception::type_error(scope, message);
  v8::Global::new(scope, exception)
}

/// Implements `value instanceof primordials.Error` in JS. Similar to
/// `Value::is_native_error()` but more closely matches the semantics
/// of `instanceof`. `Value::is_native_error()` also checks for static class
/// inheritance rather than just scanning the prototype chain, which doesn't
/// work with our WebIDL implementation of `DOMException`.
pub(crate) fn is_instance_of_error(
  scope: &mut v8::HandleScope,
  value: v8::Local<v8::Value>,
) -> bool {
  if !value.is_object() {
    return false;
  }
  let message = v8::String::empty(scope);
  let error_prototype = v8::Exception::error(scope, message)
    .to_object(scope)
    .unwrap()
    .get_prototype(scope)
    .unwrap();
  let mut maybe_prototype =
    value.to_object(scope).unwrap().get_prototype(scope);
  while let Some(prototype) = maybe_prototype {
    if !prototype.is_object() {
      return false;
    }
    if prototype.strict_equals(error_prototype) {
      return true;
    }
    maybe_prototype = prototype
      .to_object(scope)
      .and_then(|o| o.get_prototype(scope));
  }
  false
}

/// Implements `value instanceof primordials.AggregateError` in JS,
/// by walking the prototype chain, and comparing each links constructor `name` property.
///
/// NOTE: There is currently no way to detect `AggregateError` via `rusty_v8`,
/// as v8 itself doesn't expose `v8__Exception__AggregateError`,
/// and we cannot create bindings for it. This forces us to rely on `name` inference.
pub(crate) fn is_aggregate_error(
  scope: &mut v8::HandleScope,
  value: v8::Local<v8::Value>,
) -> bool {
  let mut maybe_prototype = Some(value);
  while let Some(prototype) = maybe_prototype {
    if !prototype.is_object() {
      return false;
    }

    let prototype = prototype.to_object(scope).unwrap();
    let prototype_name =
      match get_property(scope, prototype, v8_static_strings::CONSTRUCTOR) {
        Some(constructor) => {
          let ctor = constructor.to_object(scope).unwrap();
          get_property(scope, ctor, v8_static_strings::NAME)
            .map(|v| v.to_rust_string_lossy(scope))
        }
        None => return false,
      };

    if prototype_name == Some(String::from("AggregateError")) {
      return true;
    }

    maybe_prototype = prototype.get_prototype(scope);
  }

  false
}

/// Check if the error has a proper stack trace. The stack trace checked is the
/// one passed to `prepareStackTrace()`, not `msg.get_stack_trace()`.
pub(crate) fn has_call_site(
  scope: &mut v8::HandleScope,
  exception: v8::Local<v8::Value>,
) -> bool {
  if !exception.is_object() {
    return false;
  }
  let exception = exception.to_object(scope).unwrap();
  // Access error.stack to ensure that prepareStackTrace() has been called.
  // This should populate error.#callSiteEvals.
  get_property(scope, exception, v8_static_strings::STACK);
  let frames_v8 = {
    let key = call_site_evals_key(scope);
    exception.get_private(scope, key)
  };
  let frames_v8: Option<v8::Local<v8::Array>> =
    frames_v8.and_then(|a| a.try_into().ok());
  if let Some(frames_v8) = frames_v8 {
    if frames_v8.length() > 0 {
      return true;
    }
  }
  false
}

const DATA_URL_ABBREV_THRESHOLD: usize = 150;

pub fn format_file_name(file_name: &str) -> String {
  abbrev_file_name(file_name).unwrap_or_else(|| {
    // same as to_percent_decoded_str() in cli/util/path.rs
    match percent_encoding::percent_decode_str(file_name).decode_utf8() {
      Ok(s) => s.to_string(),
      // when failing as utf-8, just return the original string
      Err(_) => file_name.to_string(),
    }
  })
}

fn abbrev_file_name(file_name: &str) -> Option<String> {
  if !file_name.starts_with("data:") {
    return None;
  }
  if file_name.len() <= DATA_URL_ABBREV_THRESHOLD {
    return Some(file_name.to_string());
  }
  let url = Url::parse(file_name).ok()?;
  let (head, tail) = url.path().split_once(',')?;
  let len = tail.len();
  let start = tail.get(0..20)?;
  let end = tail.get(len - 20..)?;
  Some(format!("{}:{},{}......{}", url.scheme(), head, start, end))
}

pub(crate) fn exception_to_err_result<T>(
  scope: &mut v8::HandleScope,
  exception: v8::Local<v8::Value>,
  mut in_promise: bool,
  clear_error: bool,
) -> Result<T, Error> {
  let state = JsRealm::exception_state_from_scope(scope);

  let mut was_terminating_execution = scope.is_execution_terminating();

  // Disable running microtasks for a moment. When upgrading to V8 v11.4
  // we discovered that canceling termination here will cause the queued
  // microtasks to run which breaks some tests.
  scope.set_microtasks_policy(v8::MicrotasksPolicy::Explicit);
  // If TerminateExecution was called, cancel isolate termination so that the
  // exception can be created. Note that `scope.is_execution_terminating()` may
  // have returned false if TerminateExecution was indeed called but there was
  // no JS to execute after the call.
  scope.cancel_terminate_execution();
  let exception = if let Some(dispatched_exception) =
    state.get_dispatched_exception_as_local(scope)
  {
    // If termination is the result of a `reportUnhandledException` call, we want
    // to use the exception that was passed to it rather than the exception that
    // was passed to this function.
    in_promise = state.is_dispatched_exception_promise();
    if clear_error {
      state.clear_error();
      was_terminating_execution = false;
    }
    dispatched_exception
  } else if was_terminating_execution && exception.is_null_or_undefined() {
    // If we are terminating and there is no exception, throw `new Error("execution terminated")``.
    let message = v8::String::new(scope, "execution terminated").unwrap();
    v8::Exception::error(scope, message)
  } else {
    // Otherwise re-use the exception
    exception
  };

  let mut js_error = JsError::from_v8_exception(scope, exception);
  if in_promise {
    js_error.exception_message = format!(
      "Uncaught (in promise) {}",
      js_error.exception_message.trim_start_matches("Uncaught ")
    );
  }

  if was_terminating_execution {
    // Resume exception termination.
    scope.terminate_execution();
  }
  scope.set_microtasks_policy(v8::MicrotasksPolicy::Auto);

  Err(js_error.into())
}

pub fn throw_type_error(scope: &mut v8::HandleScope, message: impl AsRef<str>) {
  let message = v8::String::new(scope, message.as_ref()).unwrap();
  let exception = v8::Exception::type_error(scope, message);
  scope.throw_exception(exception);
}

v8_static_strings::v8_static_strings! {
  ERROR = "Error",
  GET_FILE_NAME = "getFileName",
  GET_THIS = "getThis",
  GET_TYPE_NAME = "getTypeName",
  GET_FUNCTION = "getFunction",
  GET_FUNCTION_NAME = "getFunctionName",
  GET_METHOD_NAME = "getMethodName",
  GET_LINE_NUMBER = "getLineNumber",
  GET_COLUMN_NUMBER = "getColumnNumber",
  GET_EVAL_ORIGIN = "getEvalOrigin",
  IS_TOPLEVEL = "isToplevel",
  IS_EVAL = "isEval",
  IS_NATIVE = "isNative",
  IS_CONSTRUCTOR = "isConstructor",
  IS_ASYNC = "isAsync",
  IS_PROMISE_ALL = "isPromiseAll",
  GET_PROMISE_INDEX = "getPromiseIndex",
  PREPARE_STACK_TRACE = "prepareStackTrace",
  ORIGINAL = "_orig",
  DEFAULT_PREPARE = "defaultPrepareStackTrace",
}

trait Cast<'s, T>: Sized {
  fn cast<O>(self) -> Result<v8::Local<'s, O>, v8::DataError>
  where
    v8::Local<'s, T>: TryInto<v8::Local<'s, O>, Error = v8::DataError>;
  fn casted<O>(self) -> v8::Local<'s, O>
  where
    v8::Local<'s, T>: TryInto<v8::Local<'s, O>, Error: std::fmt::Debug>;
}

impl<'s, T> Cast<'s, T> for v8::Local<'s, T> {
  fn cast<O>(self) -> Result<v8::Local<'s, O>, v8::DataError>
  where
    v8::Local<'s, T>: TryInto<v8::Local<'s, O>, Error = v8::DataError>,
  {
    self.try_into()
  }
  fn casted<O>(self) -> v8::Local<'s, O>
  where
    v8::Local<'s, T>: TryInto<v8::Local<'s, O>, Error: std::fmt::Debug>,
  {
    self.try_into().unwrap()
  }
}

#[inline(always)]
pub(crate) fn original_call_site_key<'a>(
  scope: &mut v8::HandleScope<'a>,
) -> v8::Local<'a, v8::Private> {
  let name = ORIGINAL.v8_string(scope);
  v8::Private::for_api(scope, Some(name))
}

fn make_patched_callsite<'s>(
  scope: &mut v8::HandleScope<'s>,
  callsite: v8::Local<'s, v8::Object>,
  template: v8::Local<'s, v8::ObjectTemplate>,
) -> v8::Local<'s, v8::Object> {
  let out_obj = template.new_instance(scope).unwrap();
  let orig_key = original_call_site_key(scope);
  out_obj.set_private(scope, orig_key, callsite.into());
  out_obj
}

macro_rules! make_delegate {
  ($scope: ident, $template: ident, [$($field: ident),+ $(,)?]) => {
    $(
      {
        let key = $field.v8_string($scope).into();
        $template.set(
          key,
          v8::FunctionBuilder::<v8::FunctionTemplate>::new(
            |scope: &mut v8::HandleScope<'_>,
            args: v8::FunctionCallbackArguments<'_>,
            mut rv: v8::ReturnValue<'_>| {
              let orig_key = original_call_site_key(scope);
              let orig = args.this().get_private(scope, orig_key).unwrap();
              let key = $field.v8_string(scope).into();
              let orig_ret = orig
                .casted::<v8::Object>()
                .get(scope, key)
                .unwrap()
                .casted::<v8::Function>()
                .call(scope, orig, &[]);
              rv.set(orig_ret.unwrap_or_else(|| v8::undefined(scope).into()));
            },
          )
          .build($scope)
          .into(),
        );
      }
    )+
  };
}

pub(crate) fn make_callsite_template<'s>(
  scope: &mut v8::HandleScope<'s>,
) -> v8::Local<'s, v8::ObjectTemplate> {
  let template = v8::ObjectTemplate::new(scope);

  make_delegate!(
    scope,
    template,
    [
      // excludes getFileName, which we'll override below
      GET_THIS,
      GET_TYPE_NAME,
      GET_FUNCTION,
      GET_FUNCTION_NAME,
      GET_METHOD_NAME,
      GET_LINE_NUMBER,
      GET_COLUMN_NUMBER,
      GET_EVAL_ORIGIN,
      IS_TOPLEVEL,
      IS_EVAL,
      IS_NATIVE,
      IS_CONSTRUCTOR,
      IS_ASYNC,
      IS_PROMISE_ALL,
      GET_PROMISE_INDEX,
    ]
  );

  let get_file_name_key = GET_FILE_NAME.v8_string(scope).into();
  template.set(
    get_file_name_key,
    v8::FunctionBuilder::<v8::FunctionTemplate>::new(
      |scope: &mut v8::HandleScope<'_>,
       args: v8::FunctionCallbackArguments<'_>,
       mut rv: v8::ReturnValue<'_>| {
        let mut inner = || -> Option<()> {
          let orig_key = original_call_site_key(scope);
          let orig = args.this().get_private(scope, orig_key).unwrap();
          let key = GET_FILE_NAME.v8_string(scope).into();
          let orig_ret = orig
            .casted::<v8::Object>()
            .get(scope, key)?
            .casted::<v8::Function>()
            .call(scope, orig, &[]);
          if let Some(ret_val) = orig_ret {
            let string = ret_val.to_rust_string_lossy(scope);
            let file_name = if string.starts_with("file://") {
              Url::parse(&string)
                .ok()?
                .to_file_path()
                .ok()?
                .to_string_lossy()
                .into_owned()
            } else {
              string
            };
            let v8_str = crate::FastString::from(file_name).v8_string(scope);
            rv.set(v8_str.into());
          }
          Some(())
        };
        inner().unwrap();
      },
    )
    .build(scope)
    .into(),
  );

  template
}

pub fn prepare_stack_trace_callback<'s>(
  scope: &mut v8::HandleScope<'s>,
  error: v8::Local<'s, v8::Value>,
  callsites: v8::Local<'s, v8::Array>,
) -> v8::Local<'s, v8::Value> {
  let global = scope.get_current_context().global(scope);
  let error_key = ERROR.v8_string(scope);
  let prepare_stack_trace_key = PREPARE_STACK_TRACE.v8_string(scope);
  let global_error = global
    .get(scope, error_key.into())
    .unwrap()
    .cast::<v8::Object>()
    .unwrap();
  let prepare_fn = global_error
    .get(scope, prepare_stack_trace_key.into())
    .and_then(|v| v.cast::<v8::Function>().ok());
  if let Some(prepare_fn) = prepare_fn {
    let len = callsites.length();
    let mut patched = Vec::with_capacity(len as usize);
    let template = JsRuntime::state_from(scope)
      .callsite_template
      .borrow()
      .clone()
      .unwrap();
    let template = v8::Local::new(scope, template);
    for i in 0..len {
      let callsite = callsites
        .get_index(scope, i)
        .unwrap()
        .cast::<v8::Object>()
        .unwrap();
      patched.push(make_patched_callsite(scope, callsite, template).into());
    }
    let patched_callsites = v8::Array::new_with_elements(scope, &patched);

    let this = global_error.into();
    let args = [error.into(), patched_callsites.into()];
    return prepare_fn
      .call(scope, this, &args)
      .unwrap_or_else(|| v8::undefined(scope).into());
  }

  let default_key = DEFAULT_PREPARE.v8_string(scope);
  let default_prepare = global
    .get(scope, default_key.into())
    .unwrap()
    .casted::<v8::Function>();
  let undef = v8::undefined(scope).into();
  default_prepare
    .call(scope, undef, &[error, callsites.into()])
    .unwrap()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_bad_resource() {
    let err = bad_resource("Resource has been closed");
    assert_eq!(err.to_string(), "Resource has been closed");
  }

  #[test]
  fn test_bad_resource_id() {
    let err = bad_resource_id();
    assert_eq!(err.to_string(), "Bad resource ID");
  }

  #[test]
  fn test_format_file_name() {
    let file_name = format_file_name("data:,Hello%2C%20World%21");
    assert_eq!(file_name, "data:,Hello%2C%20World%21");

    let too_long_name = "a".repeat(DATA_URL_ABBREV_THRESHOLD + 1);
    let file_name = format_file_name(&format!(
      "data:text/plain;base64,{too_long_name}_%F0%9F%A6%95"
    ));
    assert_eq!(
      file_name,
      "data:text/plain;base64,aaaaaaaaaaaaaaaaaaaa......aaaaaaa_%F0%9F%A6%95"
    );

    let file_name = format_file_name("file:///foo/bar.ts");
    assert_eq!(file_name, "file:///foo/bar.ts");

    let file_name =
      format_file_name("file:///%E6%9D%B1%E4%BA%AC/%F0%9F%A6%95.ts");
    assert_eq!(file_name, "file:///東京/🦕.ts");
  }
}
