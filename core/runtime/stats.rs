// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use bit_set::BitSet;
use serde::Serialize;
use crate::OpState;
use crate::ResourceId;
use crate::PromiseId;
use crate::OpDecl;
use std::cell::RefCell;
use std::rc::Rc;
use super::ContextState;
use super::op_driver::OpDriver;
use super::op_driver::OpInflightStats;

#[derive(Clone)]
pub struct RuntimeActivityStatsFactory {
  pub(super) context_state: Rc<ContextState>,
  pub(super) op_state: Rc<RefCell<OpState>>,
}

impl RuntimeActivityStatsFactory {
  pub fn capture(self) -> RuntimeActivityStats {
    let mut resources = ResourceOpenStats { resources: vec![] };
    for resource in self.op_state.borrow().resource_table.names() {
      resources.resources.push((resource.0, resource.1.to_owned().to_string()))
    }
    let timers = TimerStats {
      intervals: vec![],
      timers: vec![]
    };
    RuntimeActivityStats {
      context_state: self.context_state.clone(),
      op: self.context_state.pending_ops.stats(),
      resources,
      timers
    }
  }
}

pub struct ResourceOpenStats {
  pub(super) resources: Vec<(u32, String)>
}

pub struct TimerStats {
  pub(super) timers: Vec<usize>,
  pub(super) intervals: Vec<usize>,
}

/// Information about in-flight ops, open resources, active timers and other runtime-specific
/// data that can be used for test sanitization.
pub struct RuntimeActivityStats {
  context_state: Rc<ContextState>,
  pub(super) op: OpInflightStats,
  pub(super) resources: ResourceOpenStats,
  pub(super) timers: TimerStats,
}

#[derive(Serialize)]
pub enum RuntimeActivity {
  AsyncOp(PromiseId, String),
  Resource(ResourceId, String),
  Timer(usize, usize),
  Interval(usize, usize),
}

impl RuntimeActivityStats {
  /// Capture the data within this [`RuntimeActivityStats`] as a [`RuntimeActivitySnapshot`]
  /// with details of activity.
  pub fn dump(&self) -> RuntimeActivitySnapshot {
    let mut v = vec![];
    let ops = self.context_state.op_ctxs.borrow();
    for op in self.op.ops.iter() {
      v.push(RuntimeActivity::AsyncOp(op.0, ops[op.1 as usize].decl.name.to_owned()));
    }
    RuntimeActivitySnapshot { active: v }
  }

  pub fn diff(before: &Self, after: &Self) -> RuntimeActivityDiff {
    let mut appeared = vec![];
    let mut disappeared = vec![];
    let ops = before.context_state.op_ctxs.borrow();

    let mut a = BitSet::new();
    for op in after.op.ops.iter() {
      a.insert(op.0 as usize);
    }
    for op in before.op.ops.iter() {
      if a.remove(op.0 as usize) {
        // continuing op
      } else {
        // before, but not after
        disappeared.push(RuntimeActivity::AsyncOp(op.0, ops[op.1 as usize].decl.name.to_owned()));
      }
    }
    for op in after.op.ops.iter() {
      if a.contains(op.0 as usize) {
        // after but not before
        appeared.push(RuntimeActivity::AsyncOp(op.0, ops[op.1 as usize].decl.name.to_owned()));
      }
    }
    
    RuntimeActivityDiff { appeared, disappeared }
  }
}

#[derive(Serialize)]
pub struct RuntimeActivityDiff {
  pub appeared: Vec<RuntimeActivity>,
  pub disappeared: Vec<RuntimeActivity>,
}

#[derive(Serialize)]
pub struct RuntimeActivitySnapshot {
  pub active: Vec<RuntimeActivity>,
}
