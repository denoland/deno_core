// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use crate::GetErrorClassFn;
use crate::OpId;
use crate::PromiseId;
use anyhow::Error;
use std::future::Future;
use std::task::Context;
use std::task::Poll;

mod erased_future;
mod future_arena;
mod futures_unordered_driver;
mod joinset_driver;
mod op_results;
mod submission_queue;

pub use futures_unordered_driver::FuturesUnorderedDriver;
pub use joinset_driver::JoinSetDriver;

pub use self::op_results::OpMappingContext;
use self::op_results::OpResult;
pub use self::op_results::V8OpMappingContext;
pub use self::op_results::V8RetValMapper;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpScheduling {
  Eager,
  Lazy,
  Deferred,
}

/// `OpDriver` encapsulates the interface for handling operations within Deno's runtime.
///
/// This trait defines methods for submitting ops and polling readiness inside of the
/// event loop.
///
/// The driver takes an optional [`OpMappingContext`] implementation, which defaults to
/// one compatible with v8. This is used solely for testing purposes.
pub(crate) trait OpDriver<C: OpMappingContext = V8OpMappingContext>:
  Default
{
  /// Submits an operation that is expected to complete successfully without errors.
  fn submit_op_infallible<R: 'static, const LAZY: bool, const DEFERRED: bool>(
    &self,
    op_id: OpId,
    metrics_enabled: bool,
    promise_id: i32,
    op: impl Future<Output = R> + 'static,
    rv_map: C::MappingFn<R>,
  ) -> Option<R>;

  /// Submits an operation that is expected to complete successfully without errors.
  fn submit_op_infallible_scheduling<R: 'static>(
    &self,
    scheduling: OpScheduling,
    op_id: OpId,
    metrics_enabled: bool,
    promise_id: i32,
    op: impl Future<Output = R> + 'static,
    rv_map: C::MappingFn<R>,
  ) -> Option<R> {
    match scheduling {
      OpScheduling::Eager => self.submit_op_infallible::<R, false, false>(
        op_id,
        metrics_enabled,
        promise_id,
        op,
        rv_map,
      ),
      OpScheduling::Lazy => self.submit_op_infallible::<R, true, false>(
        op_id,
        metrics_enabled,
        promise_id,
        op,
        rv_map,
      ),
      OpScheduling::Deferred => self.submit_op_infallible::<R, false, true>(
        op_id,
        metrics_enabled,
        promise_id,
        op,
        rv_map,
      ),
    }
  }

  /// Submits an operation that may produce errors during execution.
  ///
  /// This method is similar to `submit_op_infallible` but is used when the op
  /// might return an error (`Result`).
  fn submit_op_fallible<
    R: 'static,
    E: Into<Error> + 'static,
    const LAZY: bool,
    const DEFERRED: bool,
  >(
    &self,
    op_id: OpId,
    metrics_enabled: bool,
    get_class: GetErrorClassFn,
    promise_id: i32,
    op: impl Future<Output = Result<R, E>> + 'static,
    rv_map: C::MappingFn<R>,
  ) -> Option<Result<R, E>>;

  /// Submits an operation that is expected to complete successfully without errors.
  fn submit_op_fallible_scheduling<R: 'static, E: Into<Error> + 'static>(
    &self,
    scheduling: OpScheduling,
    op_id: OpId,
    metrics_enabled: bool,
    get_class: GetErrorClassFn,
    promise_id: i32,
    op: impl Future<Output = Result<R, E>> + 'static,
    rv_map: C::MappingFn<R>,
  ) -> Option<Result<R, E>> {
    match scheduling {
      OpScheduling::Eager => self.submit_op_fallible::<R, E, false, false>(
        op_id,
        metrics_enabled,
        get_class,
        promise_id,
        op,
        rv_map,
      ),
      OpScheduling::Lazy => self.submit_op_fallible::<R, E, true, false>(
        op_id,
        metrics_enabled,
        get_class,
        promise_id,
        op,
        rv_map,
      ),
      OpScheduling::Deferred => self.submit_op_fallible::<R, E, false, true>(
        op_id,
        metrics_enabled,
        get_class,
        promise_id,
        op,
        rv_map,
      ),
    }
  }

  #[allow(clippy::type_complexity)]
  /// Polls the readiness of the op driver.
  fn poll_ready<'s>(
    &self,
    cx: &mut Context,
  ) -> Poll<(PromiseId, OpId, bool, OpResult<C>)>;

  fn len(&self) -> usize;
}

#[cfg(test)]
mod tests {
  use std::future::poll_fn;

  use super::op_results::*;
  use super::*;
  use rstest::rstest;

  struct TestMappingContext {}
  impl<'s> OpMappingContextLifetime<'s> for TestMappingContext {
    type Context = ();
    type Result = String;
    type MappingError = anyhow::Error;

    fn map_error(
      context: &mut Self::Context,
      err: op_results::OpError,
    ) -> UnmappedResult<'s, Self> {
      Ok(format!("{err:?}"))
    }

    fn map_mapping_error(
      context: &mut Self::Context,
      err: Self::MappingError,
    ) -> Self::Result {
      format!("{err:?}")
    }
  }
  impl OpMappingContext for TestMappingContext {
    type MappingFn<R: 'static> = for<'s> fn(R) -> Result<String, anyhow::Error>;
    fn erase_mapping_fn<R: 'static>(f: Self::MappingFn<R>) -> *const fn() {
      unsafe { std::mem::transmute(f) }
    }

    fn unerase_mapping_fn<'s, R: 'static>(
      f: *const fn(),
      scope: &mut <Self as OpMappingContextLifetime<'s>>::Context,
      r: R,
    ) -> UnmappedResult<'s, Self> {
      let f: Self::MappingFn<R> = unsafe { std::mem::transmute(f) };
      f(r)
    }
  }

  #[rstest]
  #[case::joinset(JoinSetDriver::<TestMappingContext>::default())]
  #[case::futures_unordered(FuturesUnorderedDriver::<TestMappingContext>::default())]
  fn test_driver<D: OpDriver<TestMappingContext>>(
    #[case] driver: D,
    #[values(2, 16)] count: usize,
    #[values(OpScheduling::Eager, OpScheduling::Lazy, OpScheduling::Deferred)]
    scheduling: OpScheduling,
  ) {
    let runtime = tokio::runtime::Builder::new_current_thread()
      .build()
      .unwrap();
    runtime.block_on(async {
      for i in 0..count {
        let res = driver.submit_op_infallible_scheduling(
          scheduling,
          0,
          false,
          0,
          async { 1 },
          |r| Ok(format!("{r}")),
        );
        if scheduling == OpScheduling::Eager {
          assert_eq!(Some(1), res);
        } else {
          assert_eq!(None, res);
        }
      }
      if scheduling == OpScheduling::Eager {
        return;
      }
      for i in 0..count {
        let (promise_id, op_id, metrics, result) =
          poll_fn(|cx| driver.poll_ready(cx)).await;
        assert_eq!(0, promise_id);
        assert_eq!(0, op_id);
        assert_eq!(false, metrics);
        assert_eq!("1", &(result.unwrap(&mut ()).unwrap()));
      }
    });
  }

  #[rstest]
  #[case::joinset(JoinSetDriver::<TestMappingContext>::default())]
  #[case::futures_unordered(FuturesUnorderedDriver::<TestMappingContext>::default())]
  fn test_driver_yield<D: OpDriver<TestMappingContext>>(
    #[case] driver: D,
    #[values(2, 16)] count: usize,
    #[values(OpScheduling::Eager, OpScheduling::Lazy, OpScheduling::Deferred)]
    scheduling: OpScheduling,
  ) {
    async fn task() -> usize {
      let v = [0_u8, 1, 2, 3];
      for i in &v {
        for _ in 0..*i {
          tokio::task::yield_now().await;
        }
      }
      v.len()
    }

    let runtime = tokio::runtime::Builder::new_current_thread()
      .build()
      .unwrap();
    runtime.block_on(async {
      for i in 0..count {
        assert_eq!(
          None,
          driver.submit_op_infallible_scheduling(
            scheduling,
            0,
            false,
            0,
            task(),
            |r| { Ok(format!("{r}")) }
          )
        );
      }
      for i in 0..count {
        let (promise_id, op_id, metrics, result) =
          poll_fn(|cx| driver.poll_ready(cx)).await;
        assert_eq!(0, promise_id);
        assert_eq!(0, op_id);
        assert_eq!(false, metrics);
        assert_eq!("4", &(result.unwrap(&mut ()).unwrap()));
      }
    });
  }

  #[rstest]
  #[case::joinset(JoinSetDriver::<TestMappingContext>::default())]
  #[case::futures_unordered(FuturesUnorderedDriver::<TestMappingContext>::default())]
  fn test_driver_large<D: OpDriver<TestMappingContext>>(
    #[case] driver: D,
    #[values(2, 16)] count: usize,
    #[values(OpScheduling::Eager, OpScheduling::Lazy, OpScheduling::Deferred)]
    scheduling: OpScheduling,
  ) {
    async fn task() -> i32 {
      let mut v = [0; 1024];
      for i in 0..10 {
        tokio::task::yield_now().await;
        v[i] = 1;
      }
      let mut s = 0;
      for i in v {
        s += i;
      }
      s
    }

    let runtime = tokio::runtime::Builder::new_current_thread()
      .build()
      .unwrap();
    runtime.block_on(async {
      for i in 0..2 {
        assert_eq!(
          None,
          driver.submit_op_infallible_scheduling(
            scheduling,
            0,
            false,
            0,
            task(),
            |r| { Ok(format!("{r}")) }
          )
        );
      }
      for i in 0..2 {
        let (promise_id, op_id, metrics, result) =
          poll_fn(|cx| driver.poll_ready(cx)).await;
        assert_eq!(0, promise_id);
        assert_eq!(0, op_id);
        assert_eq!(false, metrics);
        assert_eq!("10", &(result.unwrap(&mut ()).unwrap()));
      }
    });
  }
}
