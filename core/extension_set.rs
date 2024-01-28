// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.

use std::cell::RefCell;
use std::rc::Rc;

use crate::extensions::Extension;
use crate::extensions::GlobalObjectMiddlewareFn;
use crate::extensions::GlobalTemplateMiddlewareFn;
use crate::extensions::OpMiddlewareFn;
use crate::ops::OpCtx;
use crate::runtime::JsRuntimeState;
use crate::runtime::RealOpDriver;
use crate::GetErrorClassFn;
use crate::OpDecl;
use crate::OpMetricsFactoryFn;
use crate::OpState;

#[derive(Debug, PartialEq)]
enum ExtensionSetState {
  Created,
  OpsConsumed,
  OpStateConsumed,
  SourceCodeAvailable,
}

pub struct ExtensionSet {
  deno_core_ext: Extension,
  extensions: Vec<Extension>,
  // TODO(bartlomieju): make it debug only
  state: ExtensionSetState,
}

impl ExtensionSet {
  pub fn new(deno_core_ext: Extension, extensions: Vec<Extension>) -> Self {
    Self {
      deno_core_ext,
      extensions,
      state: ExtensionSetState::Created,
    }
  }

  fn advance_state(
    &mut self,
    assert_state: ExtensionSetState,
    advance_to: ExtensionSetState,
  ) {
    if self.state != assert_state {
      panic!(
        "Wrong ExtensionSetState state: expected {:?}, got {:?}",
        assert_state, self.state
      );
    }
    self.state = advance_to;
  }

  pub fn consume(mut self) -> Vec<Extension> {
    self.advance_state(
      ExtensionSetState::OpStateConsumed,
      ExtensionSetState::SourceCodeAvailable,
    );
    self.extensions
  }
}

/// Contribute to the `OpState` from each extension.
pub fn setup_op_state(
  extension_set: &mut ExtensionSet,
  op_state: &mut OpState,
) {
  extension_set.deno_core_ext.take_state(op_state);
  for ext in extension_set.extensions.iter_mut() {
    ext.take_state(op_state);
  }
  extension_set.advance_state(
    ExtensionSetState::OpsConsumed,
    ExtensionSetState::OpStateConsumed,
  );
}

// TODO(bartlomieju): `deno_core_ext` ops should be returned as a separate
// vector - they need to be special cased and attached to `Deno.core.ops`,
// but not added to "ext:core/ops" virtual module.
/// Collects ops from extensions & applies middleware
pub fn init_ops(extension_set: &mut ExtensionSet) -> Vec<OpDecl> {
  // In debug build verify there that inter-Extension dependencies
  // are setup correctly.
  #[cfg(debug_assertions)]
  check_extensions_dependencies(
    &extension_set.deno_core_ext,
    &extension_set.extensions,
  );

  let no_of_ops = extension_set
    .extensions
    .iter()
    .map(|e| e.op_count())
    .fold(0, |ext_ops_count, count| count + ext_ops_count);
  let mut ops =
    Vec::with_capacity(no_of_ops + extension_set.deno_core_ext.op_count());

  // Collect all middlewares - deno_core extension must not have a middleware!
  let middlewares: Vec<Box<OpMiddlewareFn>> = extension_set
    .extensions
    .iter_mut()
    .filter_map(|e| e.take_middleware())
    .collect();

  // Create a single macroware out of all middleware functions.
  let macroware = move |d| middlewares.iter().fold(d, |d, m| m(d));

  // Collect ops from all extensions and apply a macroware to each of them.
  let ext_ops = extension_set.deno_core_ext.init_ops();
  for ext_op in ext_ops {
    ops.push(OpDecl {
      name: ext_op.name,
      ..macroware(*ext_op)
    });
  }

  for ext in extension_set.extensions.iter_mut() {
    let ext_ops = ext.init_ops();
    for ext_op in ext_ops {
      ops.push(OpDecl {
        name: ext_op.name,
        ..macroware(*ext_op)
      });
    }
  }

  // In debug build verify there are no duplicate ops.
  #[cfg(debug_assertions)]
  check_no_duplicate_op_names(&ops);

  extension_set
    .advance_state(ExtensionSetState::Created, ExtensionSetState::OpsConsumed);

  ops
}

/// This functions panics if any of the extensions is missing its dependencies.
#[cfg(debug_assertions)]
fn check_extensions_dependencies(
  deno_core_ext: &Extension,
  exts: &[Extension],
) {
  for (index, ext) in exts.iter().enumerate() {
    let mut previous_exts = vec![deno_core_ext];
    previous_exts.extend_from_slice(&exts[..index].iter().collect::<Vec<_>>());
    ext.check_dependencies(&previous_exts);
  }
}

/// This function panics if there are ops with duplicate names
#[cfg(debug_assertions)]
fn check_no_duplicate_op_names(ops: &[OpDecl]) {
  use std::collections::HashMap;

  let mut count_by_name = HashMap::new();

  for op in ops.iter() {
    count_by_name
      .entry(&op.name)
      .or_insert(vec![])
      .push(op.name.to_string());
  }

  let mut duplicate_ops = vec![];
  for (op_name, _count) in count_by_name.iter().filter(|(_k, v)| v.len() > 1) {
    duplicate_ops.push(op_name.to_string());
  }
  if !duplicate_ops.is_empty() {
    let mut msg = "Found ops with duplicate names:\n".to_string();
    for op_name in duplicate_ops {
      msg.push_str(&format!("  - {}\n", op_name));
    }
    msg.push_str("Op names need to be unique.");
    panic!("{}", msg);
  }
}

pub fn create_op_ctxs(
  op_decls: Vec<OpDecl>,
  op_metrics_factory_fn: Option<OpMetricsFactoryFn>,
  op_driver: Rc<RealOpDriver>,
  op_state: Rc<RefCell<OpState>>,
  runtime_state: Rc<JsRuntimeState>,
  get_error_class_fn: GetErrorClassFn,
) -> Box<[OpCtx]> {
  let op_count = op_decls.len();
  let mut op_ctxs = Vec::with_capacity(op_count);

  for (index, decl) in op_decls.into_iter().enumerate() {
    let metrics_fn = op_metrics_factory_fn
      .as_ref()
      .and_then(|f| (f)(index as _, op_count, &decl));

    let op_ctx = OpCtx::new(
      index as _,
      std::ptr::null_mut(),
      op_driver.clone(),
      decl,
      op_state.clone(),
      runtime_state.clone(),
      get_error_class_fn,
      metrics_fn,
    );

    op_ctxs.push(op_ctx);
  }

  op_ctxs.into_boxed_slice()
}

pub fn get_middlewares_and_external_refs(
  extension_set: &mut ExtensionSet,
) -> (
  Vec<GlobalTemplateMiddlewareFn>,
  Vec<GlobalObjectMiddlewareFn>,
  Vec<v8::ExternalReference<'static>>,
) {
  // TODO(bartlomieju): these numbers were chosen arbitrarily. This is a very
  // niche features and it's unlikely a lot of extensions use it.
  let mut global_template_middlewares = Vec::with_capacity(16);
  let mut global_object_middlewares = Vec::with_capacity(16);
  let mut additional_references = Vec::with_capacity(16);

  for extension in extension_set.extensions.iter_mut() {
    if let Some(middleware) = extension.get_global_template_middleware() {
      global_template_middlewares.push(middleware);
    }
    if let Some(middleware) = extension.get_global_object_middleware() {
      global_object_middlewares.push(middleware);
    }

    additional_references
      .extend_from_slice(extension.get_external_references());
  }

  (
    global_template_middlewares,
    global_object_middlewares,
    additional_references,
  )
}
