// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.

// Everything runs in test mode
#![cfg(test)]

mod checkin;

macro_rules! unit_test {
  ($($id:ident,)*) => {
    mod unit {
      $(
        #[test]
        fn $id() {
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
          $crate::checkin::runner::run_integration_test(stringify!($id));
        }
      )*
    }
  };
}

// Test individual bits of functionality. These files are loaded from the unit/ dir.
unit_test!(encode_decode_test, microtask_test, tc39_test, test_test,);

// Test the load and run of an entire file within the `checkin` infrastructure.
// These files are loaded from the system/ dir.
integration_test!(smoke_test,);
