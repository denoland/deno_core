// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.

use std::collections::BTreeSet;
use std::fmt::Debug;

pub type ExitCb = Box<dyn Fn(&str, &str)>;
pub type WarnCb = Box<dyn Fn(&str)>;

fn exit(_feature: &str, _api_name: &str) {
  std::process::exit(70);
}

fn warn_legacy_flag(_feature: &str) {}

pub struct FeatureChecker {
  features: BTreeSet<&'static str>,
  // TODO(bartlomieju): remove once we migrate away from `--unstable` flag
  // in the CLI.
  legacy_unstable: bool,
  warn_on_legacy_unstable: bool,
  exit_cb: ExitCb,
  warn_cb: WarnCb,
}

impl Default for FeatureChecker {
  fn default() -> Self {
    Self {
      features: Default::default(),
      legacy_unstable: false,
      warn_on_legacy_unstable: false,
      exit_cb: Box::new(exit),
      warn_cb: Box::new(warn_legacy_flag),
    }
  }
}

impl Debug for FeatureChecker {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("FeatureChecker")
      .field("features", &self.features)
      .field("legacy_unstable", &self.legacy_unstable)
      .field("warn_on_legacy_unstable", &self.warn_on_legacy_unstable)
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

  pub fn set_warn_cb(&mut self, cb: WarnCb) {
    self.warn_cb = cb;
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
}
