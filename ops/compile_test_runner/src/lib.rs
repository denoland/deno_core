// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#[macro_export]
macro_rules! prelude {
  () => {
    #[allow(unused_imports)]
    use deno_ops::op2;
    #[allow(unused_imports)]
    use deno_ops::WebIDL;

    pub fn main() {}
  };
}

#[cfg(test)]
mod op2_tests {
  use std::path::PathBuf;

  // TODO(mmastrac): It's faster to do things with testing_macros::fixture?
  #[testing_macros::fixture("../op2/test_cases/compiler_pass/*.rs")]
  fn compile_test(input: PathBuf) {
    let t = trybuild::TestCases::new();
    t.pass(input);
  }

  #[rustversion::nightly]
  #[test]
  fn compile_test_all() {
    // Run all the tests on a nightly build (which should take advantage of cargo's --keep-going to
    // run in parallel: https://github.com/dtolnay/trybuild/pull/168)
    let t = trybuild::TestCases::new();
    t.pass("../op2/test_cases/**/*.rs");
    t.compile_fail("../op2/test_cases_fail/**/*.rs");
  }

  #[rustversion::not(nightly)]
  #[test]
  fn compile_test_all() {
    // Run all the tests if we're in the CI
    if let Ok(true) = std::env::var("CI").map(|s| s == "true") {
      let t = trybuild::TestCases::new();
      t.compile_fail("../op2/test_cases_fail/**/*.rs");
      t.pass("../op2/test_cases/**/*.rs");
    }
  }
}

#[cfg(test)]
mod webidl_tests {
  #[rustversion::nightly]
  #[test]
  fn compile_test_all() {
    // Run all the tests on a nightly build (which should take advantage of cargo's --keep-going to
    // run in parallel: https://github.com/dtolnay/trybuild/pull/168)
    let t = trybuild::TestCases::new();
    t.pass("../webidl/test_cases/*.rs");
    t.compile_fail("../webidl/test_cases_fail/*.rs");
  }

  #[rustversion::not(nightly)]
  #[test]
  fn compile_test_all() {
    // Run all the tests if we're in the CI
    if let Ok(true) = std::env::var("CI").map(|s| s == "true") {
      let t = trybuild::TestCases::new();
      t.compile_fail("../webidl/test_cases_fail/*.rs");
      t.pass("../webidl/test_cases/*.rs");
    }
  }
}
