// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

mod checkin;

pub use checkin::runner::create_runtime_from_snapshot;
pub use checkin::runner::snapshot::create_snapshot;

macro_rules! unit_test {
  ($($id:ident,)*) => {
    #[cfg(test)]
    mod unit {
      $(
        #[test]
        fn $id() {
          $crate::checkin::runner::testing::run_unit_test(stringify!($id));
        }
      )*
    }
  };
}

macro_rules! integration_test {
  ($($id:ident,)*) => {
    #[cfg(test)]
    mod integration {
      $(
        #[test]
        fn $id() {
          $crate::checkin::runner::testing::run_integration_test(stringify!($id));
        }
      )*
    }
  };
}

// Test individual bits of functionality. These files are loaded from the unit/ dir.
unit_test!(
  encode_decode_test,
  error_test,
  microtask_test,
  ops_async_test,
  ops_buffer_test,
  resource_test,
  serialize_deserialize_test,
  stats_test,
  task_test,
  tc39_test,
  timer_test,
  type_test,
);

// Test the load and run of an entire file within the `checkin` infrastructure.
// These files are loaded from the integration/ dir.
integration_test!(
  builtin_console_test,
  dyn_import_circular,
  dyn_import_op,
  error_async_stack,
  error_rejection_catch,
  error_rejection_order,
  error_ext_stack,
  error_with_stack,
  error_without_stack,
  main_module_handler,
  module_types,
  pending_unref_op_tla,
  smoke_test,
  timer_ref,
  timer_ref_and_cancel,
  timer_many,
  ts_types,
  worker_spawn,
  worker_terminate,
  worker_terminate_op,
);
