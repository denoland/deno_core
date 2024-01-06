// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use super::erased_future::TypeErased;
use crate::GetErrorClassFn;
use crate::OpId;
use crate::PromiseId;
use anyhow::Error;
use serde::Serialize;

const MAX_RESULT_SIZE: usize = 32;

pub type MappedResult<'s, C> = Result<
  <C as OpMappingContextLifetime<'s>>::Result,
  <C as OpMappingContextLifetime<'s>>::Result,
>;
pub type UnmappedResult<'s, C> = Result<
  <C as OpMappingContextLifetime<'s>>::Result,
  <C as OpMappingContextLifetime<'s>>::MappingError,
>;

pub trait OpMappingContextLifetime<'s> {
  type Context: 's;
  type Result: 's;
  type MappingError: 's;

  fn map_error(
    context: &mut Self::Context,
    err: OpError,
  ) -> UnmappedResult<'s, Self>;
  fn map_mapping_error(
    context: &mut Self::Context,
    err: Self::MappingError,
  ) -> Self::Result;
}

/// A type that allows for arbitrary mapping systems for op output with lifetime
/// control. We add this extra layer of generics because we want to test our drivers
/// without requiring V8.
pub trait OpMappingContext:
  for<'s> OpMappingContextLifetime<'s> + 'static
{
  type MappingFn<R: 'static>;

  fn erase_mapping_fn<R: 'static>(f: Self::MappingFn<R>) -> *const fn();
  fn unerase_mapping_fn<'s, R: 'static>(
    f: *const fn(),
    scope: &mut <Self as OpMappingContextLifetime<'s>>::Context,
    r: R,
  ) -> UnmappedResult<'s, Self>;
}

#[derive(Default)]
pub struct V8OpMappingContext {}

pub type V8RetValMapper<R> =
  for<'r> fn(
    &mut v8::HandleScope<'r>,
    R,
  ) -> Result<v8::Local<'r, v8::Value>, serde_v8::Error>;

impl<'s> OpMappingContextLifetime<'s> for V8OpMappingContext {
  type Context = v8::HandleScope<'s>;
  type Result = v8::Local<'s, v8::Value>;
  type MappingError = serde_v8::Error;

  #[inline(always)]
  fn map_error(
    scope: &mut v8::HandleScope<'s>,
    err: OpError,
  ) -> UnmappedResult<'s, Self> {
    serde_v8::to_v8(scope, err)
  }

  fn map_mapping_error(
    scope: &mut v8::HandleScope<'s>,
    err: Self::MappingError,
  ) -> v8::Local<'s, v8::Value> {
    serde_v8::to_v8(scope, OpError::new(&|_| "TypeError", err.into())).unwrap()
  }
}

impl OpMappingContext for V8OpMappingContext {
  type MappingFn<R: 'static> = V8RetValMapper<R>;

  #[inline(always)]
  fn erase_mapping_fn<R: 'static>(f: Self::MappingFn<R>) -> *const fn() {
    unsafe { std::mem::transmute(f) }
  }
  #[inline(always)]
  fn unerase_mapping_fn<'s, R: 'static>(
    f: *const fn(),
    scope: &mut <Self as OpMappingContextLifetime<'s>>::Context,
    r: R,
  ) -> Result<v8::Local<'s, v8::Value>, serde_v8::Error> {
    let f: Self::MappingFn<R> = unsafe { std::mem::transmute(f) };
    f(scope, r)
  }
}

pub struct PendingOp<C: OpMappingContext>(pub PendingOpInfo, pub OpResult<C>);

pub struct PendingOpInfo(pub PromiseId, pub OpId, pub bool);

type MapRawFn<C> = for<'a> fn(
  _lifetime: &'a (),
  scope: &mut <C as OpMappingContextLifetime<'a>>::Context,
  rv_map: *const fn(),
  value: TypeErased<MAX_RESULT_SIZE>,
) -> UnmappedResult<'a, C>;

#[allow(clippy::type_complexity)]
pub struct OpValue<C: OpMappingContext> {
  value: TypeErased<MAX_RESULT_SIZE>,
  rv_map: *const fn(),
  map_fn: MapRawFn<C>,
}

impl<C: OpMappingContext> OpValue<C> {
  pub fn new<R: 'static>(rv_map: C::MappingFn<R>, v: R) -> Self {
    Self {
      value: TypeErased::new(v),
      rv_map: C::erase_mapping_fn(rv_map),
      map_fn: |_, scope, rv_map, erased| unsafe {
        let r = erased.take();
        C::unerase_mapping_fn::<R>(rv_map, scope, r)
      },
    }
  }
}

pub trait ValueLargeFn<C: OpMappingContext> {
  fn unwrap<'a>(
    self: Box<Self>,
    ctx: &mut <C as OpMappingContextLifetime<'a>>::Context,
  ) -> UnmappedResult<'a, C>;
}

struct ValueLarge<C: OpMappingContext, R: 'static> {
  v: R,
  rv_map: C::MappingFn<R>,
}

impl<C: OpMappingContext, R> ValueLargeFn<C> for ValueLarge<C, R> {
  fn unwrap<'a>(
    self: Box<Self>,
    ctx: &mut <C as OpMappingContextLifetime<'a>>::Context,
  ) -> UnmappedResult<'a, C> {
    C::unerase_mapping_fn(C::erase_mapping_fn(self.rv_map), ctx, self.v)
  }
}

pub enum OpResult<C: OpMappingContext> {
  /// Errors.
  Err(OpError),
  /// For small ops, we include them in an erased type container.
  Value(OpValue<C>),
  /// For ops that return "large" results (> MAX_RESULT_SIZE bytes) we just box a function
  /// that can turn it into a v8 value.
  ValueLarge(Box<dyn ValueLargeFn<C>>),
}

impl<C: OpMappingContext> OpResult<C> {
  pub fn new_value<R: 'static>(v: R, rv_map: C::MappingFn<R>) -> Self {
    if std::mem::size_of::<R>() > MAX_RESULT_SIZE {
      OpResult::ValueLarge(Box::new(ValueLarge { v, rv_map }))
    } else {
      OpResult::Value(OpValue::new(rv_map, v))
    }
  }

  pub fn unwrap<'a>(
    self,
    context: &mut <C as OpMappingContextLifetime<'a>>::Context,
  ) -> MappedResult<'a, C> {
    let (success, res) = match self {
      Self::Err(err) => (false, C::map_error(context, err)),
      Self::Value(f) => (true, (f.map_fn)(&(), context, f.rv_map, f.value)),
      Self::ValueLarge(f) => (true, f.unwrap(context)),
    };
    match (success, res) {
      (true, Ok(x)) => Ok(x),
      (false, Ok(x)) => Err(x),
      (_, Err(err)) => Err(
        <C as OpMappingContextLifetime<'a>>::map_mapping_error(context, err),
      ),
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
