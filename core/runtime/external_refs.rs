// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use std::os::raw::c_void;
use v8::MapFnTo;

use super::bindings::call_console;
use super::bindings::catch_dynamic_import_promise_error;
use super::bindings::empty_fn;
use super::bindings::import_meta_resolve;
use super::bindings::op_disabled_fn;
use crate::modules::synthetic_module_evaluation_steps;

pub(crate) struct ExternalRefRegistry<'r> {
  refs: Vec<v8::ExternalReference<'r>>,
}

impl<'r> ExternalRefRegistry<'r> {
  pub fn new(no_of_ops: usize) -> Self {
    let mut registry = Self {
      // Overallocate a bit, it's better than having to resize the vector.
      refs: Vec::with_capacity(6 + (no_of_ops * 4) + 16),
    };
    registry.add_deno_core_refs();
    registry
  }

  /// Register refs for `deno_core` built-in APIs.
  fn add_deno_core_refs(&mut self) {
    self.register(
      "call_console",
      v8::ExternalReference {
        function: call_console.map_fn_to(),
      },
    );
    self.register(
      "import_meta_resolve",
      v8::ExternalReference {
        function: import_meta_resolve.map_fn_to(),
      },
    );
    self.register(
      "catch_dynamic_import_promise_error",
      v8::ExternalReference {
        function: catch_dynamic_import_promise_error.map_fn_to(),
      },
    );
    self.register(
      "empty_fn",
      v8::ExternalReference {
        function: empty_fn.map_fn_to(),
      },
    );
    self.register(
      "op_disabled_fn",
      v8::ExternalReference {
        function: op_disabled_fn.map_fn_to(),
      },
    );

    let syn_module_eval_fn: v8::SyntheticModuleEvaluationSteps =
      synthetic_module_evaluation_steps.map_fn_to();
    self.register(
      "synthetic_module_evaluation_steps",
      v8::ExternalReference {
        pointer: syn_module_eval_fn as *mut c_void,
      },
    );
  }

  pub fn register(
    &mut self,
    // TODO(bartlomieju): temporarily ununsed, but we will store this
    // name in the snapshot for verification and debugging purposes.
    _display_name: &'static str,
    ref_: v8::ExternalReference<'r>,
  ) {
    self.refs.push(ref_);
  }

  pub fn finalize(self) -> &'static v8::ExternalReferences {
    let refs = v8::ExternalReferences::new(&self.refs);
    let refs: &'static v8::ExternalReferences = Box::leak(Box::new(refs));
    refs
  }
}
