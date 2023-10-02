// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.

use std::collections::BTreeMap;

#[derive(Default, Debug)]
pub struct FeatureChecker {
  features: BTreeMap<&'static str, bool>,
  // TODO(bartlomieju): remove once we migrate away from `--unstable` flag
  // in the CLI.
  legacy_unstable: bool,
}

impl FeatureChecker {
  pub fn update(&mut self, feature: &'static str, enabled: bool) {
    let prev = self.features.insert(feature, enabled);
    assert!(prev.is_none());
  }

  /// Check if a feature is enabled.
  ///
  /// If a feature in not present in the checker, return false.
  pub fn check(&self, feature: &str) -> bool {
    *self.features.get(feature).unwrap_or(&false)
  }

  #[deprecated]  
  pub fn enable_legacy_unstable(&mut self) {
    self.legacy_unstable = true;
  }

  // TODO(bartlomieju): remove this.
  /// Check if `--unstable` flag has been passed. If not then exit the process
  /// with exit code 70.
  #[deprecated]
  pub fn check_legacy_unstable(&self, api_name: &str) {
    if !self.legacy_unstable {
      eprintln!(
        "Unstable API '{api_name}'. The --unstable flag must be provided."
      );
      std::process::exit(70);
    }
  }
}
