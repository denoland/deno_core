// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use super::op_driver::OpDriver;
use super::op_driver::OpInflightStats;
use super::ContextState;
use crate::OpState;
use crate::PromiseId;
use crate::ResourceId;
use bit_set::BitSet;
use serde::Serialize;
use std::cell::RefCell;
use std::rc::Rc;

#[derive(Clone)]
pub struct RuntimeActivityStatsFactory {
  pub(super) context_state: Rc<ContextState>,
  pub(super) op_state: Rc<RefCell<OpState>>,
}

impl RuntimeActivityStatsFactory {
  pub fn capture(self) -> RuntimeActivityStats {
    let res = &self.op_state.borrow().resource_table;
    let mut resources = ResourceOpenStats {
      resources: Vec::with_capacity(res.len()),
    };
    for resource in res.names() {
      resources
        .resources
        .push((resource.0, resource.1.to_string()))
    }
    let timer_count = self.context_state.timers.len();
    let mut timers = TimerStats {
      timers: Vec::with_capacity(timer_count),
      repeats: BitSet::with_capacity(timer_count),
    };
    for (timer_id, repeats) in &self.context_state.timers.iter() {
      if repeats {
        timers.repeats.insert(timers.timers.len());
      }
      timers.timers.push(timer_id as usize);
    }
    RuntimeActivityStats {
      context_state: self.context_state.clone(),
      op: self.context_state.pending_ops.stats(),
      resources,
      timers,
    }
  }
}

pub struct ResourceOpenStats {
  pub(super) resources: Vec<(u32, String)>,
}

pub struct TimerStats {
  pub(super) timers: Vec<usize>,
    /// `repeats` is a bitset that reports whether a given index in the ID array
    /// is an interval (if true) or a timer (if false).
    pub(super) repeats: BitSet,
}

/// Information about in-flight ops, open resources, active timers and other runtime-specific
/// data that can be used for test sanitization.
pub struct RuntimeActivityStats {
  context_state: Rc<ContextState>,
  pub(super) op: OpInflightStats,
  pub(super) resources: ResourceOpenStats,
  #[allow(dead_code)] // coming soon
  pub(super) timers: TimerStats,
}

#[derive(Serialize)]
pub enum RuntimeActivity {
  AsyncOp(PromiseId, String),
  Resource(ResourceId, String),
  Timer(usize),
  Interval(usize),
}

impl RuntimeActivityStats {
  /// Capture the data within this [`RuntimeActivityStats`] as a [`RuntimeActivitySnapshot`]
  /// with details of activity.
  pub fn dump(&self) -> RuntimeActivitySnapshot {
    let mut v = Vec::with_capacity(
      self.op.ops.len()
        + self.resources.resources.len()
        + self.timers.timers.len(),
    );
    let ops = self.context_state.op_ctxs.borrow();
    for op in self.op.ops.iter() {
      v.push(RuntimeActivity::AsyncOp(
        op.0,
        ops[op.1 as usize].decl.name.to_owned(),
      ));
    }
    for resource in self.resources.resources.iter() {
      v.push(RuntimeActivity::Resource(resource.0, resource.1.clone()))
    }
    for i in 0..self.timers.timers.len() {
      if self.timers.repeats.contains(i) {
        v.push(RuntimeActivity::Interval(self.timers.timers[i]));
      } else {
        v.push(RuntimeActivity::Timer(self.timers.timers[i]));
      }
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
        disappeared.push(RuntimeActivity::AsyncOp(
          op.0,
          ops[op.1 as usize].decl.name.to_owned(),
        ));
      }
    }
    for op in after.op.ops.iter() {
      if a.contains(op.0 as usize) {
        // after but not before
        appeared.push(RuntimeActivity::AsyncOp(
          op.0,
          ops[op.1 as usize].decl.name.to_owned(),
        ));
      }
    }

    let mut a = BitSet::new();
    for op in after.resources.resources.iter() {
      a.insert(op.0 as usize);
    }
    for op in before.resources.resources.iter() {
      if a.remove(op.0 as usize) {
        // continuing op
      } else {
        // before, but not after
        disappeared.push(RuntimeActivity::Resource(op.0, op.1.clone()));
      }
    }
    for op in after.resources.resources.iter() {
      if a.contains(op.0 as usize) {
        // after but not before
        appeared.push(RuntimeActivity::Resource(op.0, op.1.clone()));
      }
    }

    let mut a = BitSet::new();
    for timer in after.timers.timers.iter() {
      a.insert(*timer);
    }
    for index in 0..before.timers.timers.len() {
      let timer = before.timers.timers[index];
      if a.remove(timer) {
        // continuing op
      } else {
        // before, but not after
        if before.timers.repeats.contains(index) {
          disappeared.push(RuntimeActivity::Interval(timer));
        } else {
          disappeared.push(RuntimeActivity::Timer(timer));
        }
      }
    }
    for index in 0..after.timers.timers.len() {
      let timer = after.timers.timers[index];
      if a.contains(timer) {
        // after but not before
        if after.timers.repeats.contains(index) {
          appeared.push(RuntimeActivity::Interval(timer));
        } else {
          appeared.push(RuntimeActivity::Timer(timer));
        }
      }
    }

    RuntimeActivityDiff {
      appeared,
      disappeared,
    }
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
