#[macro_export]
macro_rules! prelude {
  () => {
    use deno_ops::op2;

    pub fn main() {}
  };
}

#[cfg(test)]
mod tests {
  use std::path::PathBuf;

  #[testing_macros::fixture("../op2/test_cases/**/*.rs")]
  fn compile_test(input: PathBuf) {
    let t = trybuild::TestCases::new();
    t.pass(&input);
  }
}
