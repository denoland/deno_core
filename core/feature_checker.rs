// Copyright 2018-2025 the Deno authors. MIT license.

use std::collections::BTreeSet;
use std::fmt::Debug;

pub type ExitCb = Box<dyn Fn(&str, &str) + Send + Sync>;

fn exit(feature: &str, api_name: &str) {
  #[allow(clippy::print_stderr)]
  {
    eprintln!(
      "Feature '{feature}' for '{api_name}' was not specified, exiting."
    );
  }
  std::process::exit(70);
}

pub struct FeatureChecker {
  features: BTreeSet<&'static str>,
  exit_cb: ExitCb,
}

impl Default for FeatureChecker {
  fn default() -> Self {
    Self {
      features: Default::default(),
      exit_cb: Box::new(exit),
    }
  }
}

impl Debug for FeatureChecker {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("FeatureChecker")
      .field("features", &self.features)
      .finish()
  }
}

impl FeatureChecker {
  pub fn enable_feature(&mut self, feature: &'static str) {
    let inserted = self.features.insert(feature);
    assert!(
      inserted,
      "Trying to enabled a feature that is already enabled {}",
      feature
    );
  }

  pub fn set_exit_cb(&mut self, cb: ExitCb) {
    self.exit_cb = cb;
  }

  /// Check if a feature is enabled.
  ///
  /// If a feature in not present in the checker, return false.
  #[inline(always)]
  pub fn check(&self, feature: &str) -> bool {
    self.features.contains(feature)
  }

  #[inline(always)]
  pub fn check_or_exit(&self, feature: &str, api_name: &str) {
    if !self.check(feature) {
      (self.exit_cb)(feature, api_name);
    }
  }
}

#[cfg(test)]
mod tests {
  use std::sync::atomic::AtomicUsize;
  use std::sync::atomic::Ordering;

  use super::*;

  #[test]
  fn test_feature_checker() {
    static EXIT_COUNT: AtomicUsize = AtomicUsize::new(0);

    fn exit_cb(_feature: &str, _api_name: &str) {
      EXIT_COUNT.fetch_add(1, Ordering::Relaxed);
    }

    let mut checker = FeatureChecker::default();
    checker.set_exit_cb(Box::new(exit_cb));
    checker.enable_feature("foobar");

    assert!(checker.check("foobar"));
    assert!(!checker.check("fizzbuzz"));

    checker.check_or_exit("foobar", "foo");
    assert_eq!(EXIT_COUNT.load(Ordering::Relaxed), 0);
    checker.check_or_exit("fizzbuzz", "foo");
    assert_eq!(EXIT_COUNT.load(Ordering::Relaxed), 1);
  }
}
