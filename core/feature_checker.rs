// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.

use std::collections::BTreeSet;

#[derive(Default, Debug)]
pub struct FeatureChecker {
  features: BTreeSet<&'static str>,
  // TODO(bartlomieju): remove once we migrate away from `--unstable` flag
  // in the CLI.
  legacy_unstable: bool,
}

impl FeatureChecker {
  pub fn enable_feature(&mut self, feature: &'static str) {
    self.features.insert(feature);
  }

  /// Check if a feature is enabled.
  ///
  /// If a feature in not present in the checker, return false.
  pub fn check(&self, feature: &str) -> bool {
    self.features.contains(feature)
  }

  // TODO(bartlomieju): remove this.
  pub fn enable_legacy_unstable(&mut self) {
    self.legacy_unstable = true;
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
