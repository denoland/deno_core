// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.

// Everything runs in test mode
#![cfg(test)]

use std::path::PathBuf;

mod checkin;

macro_rules! unit_test {
  ($($id:ident,)*) => {
    mod unit {
      $(
        #[test]
        fn $id() {
          $crate::compile_wat_to_wasm();
          $crate::checkin::runner::run_unit_test(stringify!($id));
        }
      )*
    }
  };
}

macro_rules! integration_test {
  ($($id:ident,)*) => {
    mod integration {
      $(
        #[test]
        fn $id() {
          $crate::compile_wat_to_wasm();
          $crate::checkin::runner::run_integration_test(stringify!($id));
        }
      )*
    }
  };
}

pub fn compile_wat_to_wasm() {
  let wat_filename = PathBuf::from("./integration/wasm_imports/add.wat");
  let contents = match std::fs::read(&wat_filename) {
    Ok(c) => c,
    Err(err) => panic!("Couldn't read {:?}, cause: {:#?}", wat_filename, err),
  };
  let wasm_bytes = match wasmer::wat2wasm(&contents) {
    Ok(b) => b,
    Err(err) => panic!("Couldn't compile wat to wasm, cause: {:#?}", err),
  };
  let wasm_filename = wat_filename.with_extension("wasm");
  if let Err(err) = std::fs::write(&wasm_filename, wasm_bytes) {
    panic!("Couldn't write {:?} file, cause: {:#?}", wasm_filename, err);
  }
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
  tc39_test,
  timer_test,
  type_test,
);

// Test the load and run of an entire file within the `checkin` infrastructure.
// These files are loaded from the system/ dir.
integration_test!(
  dyn_import_circular,
  dyn_import_op,
  error_async_stack,
  error_rejection_catch,
  error_rejection_order,
  error_with_stack,
  error_without_stack,
  smoke_test,
  timer_ref,
  timer_ref_and_cancel,
  timer_many,
  wasm_imports,
  worker_spawn,
  worker_terminate,
  worker_terminate_op,
);
