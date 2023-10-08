// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.

use std::collections::BTreeSet;

fn exit(feature: &str, api_name: &str) {
  eprintln!("Unstable API '{api_name}'. The `--unstable-{feature}` flag must be provided.");
  std::process::exit(70);
}

fn warn_legacy_flag(feature: &str) {
  eprintln!("The `--unstable` flag is deprecated, use `--unstable-{feature}` instead.");
}

#[derive(Default, Debug)]
pub struct FeatureChecker {
  features: BTreeSet<&'static str>,
  // TODO(bartlomieju): remove once we migrate away from `--unstable` flag
  // in the CLI.
  legacy_unstable: bool,
  warn_on_legacy_unstable: bool,
}

impl FeatureChecker {
  pub fn enable_feature(&mut self, feature: &'static str) {
    let inserted = self.features.insert(feature);
    assert!(inserted, "Trying to enabled a feature that is already enabled {}", feature);
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
      exit(feature, api_name);
    }
  }

  #[inline(always)]
  pub fn check_or_exit_with_legacy_fallback(
    &self,
    feature: &str,
    api_name: &str,
  ) {
    if !self.features.contains(feature) {
      if self.legacy_unstable {
        if self.warn_on_legacy_unstable {
          warn_legacy_flag(feature);
        }
        return;
      }

      exit(feature, api_name);
    }
  }

  // TODO(bartlomieju): remove this.
  pub fn enable_legacy_unstable(&mut self) {
    self.legacy_unstable = true;
  }

  // TODO(bartlomieju): remove this.
  pub fn warn_on_legacy_unstable(&mut self) {
    self.warn_on_legacy_unstable = true;
  }

  // TODO(bartlomieju): remove this.
  /// Check if `--unstable` flag has been passed. If not then exit the process
  /// with exit code 70.
  pub fn check_legacy_unstable_or_exit(&self, api_name: &str) {
    if !self.legacy_unstable {
      eprintln!(
        "Unstable API '{api_name}'. The --unstable flag must be provided."
      );
      std::process::exit(70);
    }
  }
}
