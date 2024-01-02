// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use super::erased_future::TypeErased;
use super::RetValMapper;
use crate::GetErrorClassFn;
use crate::OpId;
use crate::PromiseId;
use anyhow::Error;
use serde::Serialize;

const MAX_RESULT_SIZE: usize = 32;

pub struct PendingOp(pub PendingOpInfo, pub OpResult);

pub struct PendingOpInfo(pub PromiseId, pub OpId, pub bool);

#[allow(clippy::type_complexity)]
pub struct OpValue {
  value: TypeErased<MAX_RESULT_SIZE>,
  rv_map: *const fn(),
  map_fn: for<'a> fn(
    scope: &mut v8::HandleScope<'a>,
    rv_map: *const fn(),
    value: TypeErased<MAX_RESULT_SIZE>,
  ) -> Result<v8::Local<'a, v8::Value>, serde_v8::Error>,
}

impl OpValue {
  pub fn new<R: 'static>(rv_map: RetValMapper<R>, v: R) -> Self {
    Self {
      value: TypeErased::new(v),
      rv_map: rv_map as _,
      map_fn: |scope, rv_map, erased| unsafe {
        let r = erased.take();
        let rv_map: RetValMapper<R> = std::mem::transmute(rv_map);
        rv_map(scope, r)
      },
    }
  }
}

pub type ValueLargeFn =
  dyn for<'a> FnOnce(
    &mut v8::HandleScope<'a>,
  ) -> Result<v8::Local<'a, v8::Value>, serde_v8::Error>;

pub enum OpResult {
  /// Errors.
  Err(OpError),
  /// For small ops, we include them in an erased type container.
  Value(OpValue),
  /// For ops that return "large" results (> MAX_RESULT_SIZE bytes) we just box a function
  /// that can turn it into a v8 value.
  ValueLarge(Box<ValueLargeFn>),
}

impl OpResult {
  pub fn new_value<R: 'static>(v: R, rv_map: RetValMapper<R>) -> Self {
    if std::mem::size_of::<R>() > MAX_RESULT_SIZE {
      OpResult::ValueLarge(Box::new(move |scope| rv_map(scope, v)))
    } else {
      OpResult::Value(OpValue::new(rv_map, v))
    }
  }

  pub fn into_v8<'a>(
    self,
    scope: &mut v8::HandleScope<'a>,
  ) -> Result<v8::Local<'a, v8::Value>, serde_v8::Error> {
    match self {
      Self::Err(err) => serde_v8::to_v8(scope, err),
      Self::Value(f) => (f.map_fn)(scope, f.rv_map, f.value),
      Self::ValueLarge(f) => f(scope),
    }
  }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpError {
  #[serde(rename = "$err_class_name")]
  class_name: &'static str,
  message: String,
  code: Option<&'static str>,
}

impl OpError {
  pub fn new(get_class: GetErrorClassFn, err: Error) -> Self {
    Self {
      class_name: (get_class)(&err),
      message: format!("{err:#}"),
      code: crate::error_codes::get_error_code(&err),
    }
  }
}
