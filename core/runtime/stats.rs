// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use super::op_driver::OpDriver;
use super::op_driver::OpInflightStats;
use super::ContextState;
use crate::OpId;
use crate::OpState;
use crate::PromiseId;
use crate::ResourceId;
use bit_set::BitSet;
use serde::Serialize;
use serde::Serializer;
use std::cell::Cell;
use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Deref;
use std::rc::Rc;

#[derive(Default)]
pub struct OpCallTraces {
  enabled: Cell<bool>,
  traces: RefCell<HashMap<PromiseId, Rc<str>>>,
}

impl OpCallTraces {
  pub(crate) fn set_enabled(&self, enabled: bool) {
    self.enabled.set(enabled);
    if !enabled {
      self.traces.borrow_mut().clear();
    }
  }

  pub(crate) fn submit(&self, promise_id: PromiseId, trace: &str) {
    self.traces.borrow_mut().insert(promise_id, trace.into());
  }

  pub(crate) fn complete(&self, promise_id: PromiseId) {
    self.traces.borrow_mut().remove(&promise_id);
  }

  pub fn is_enabled(&self) -> bool {
    self.enabled.get()
  }

  pub fn count(&self) -> usize {
    self.traces.borrow().len()
  }

  pub fn get_all(&self, mut f: impl FnMut(PromiseId, &str)) {
    for (key, value) in self.traces.borrow().iter() {
      f(*key, value.as_ref())
    }
  }

  pub fn capture(&self) -> HashMap<PromiseId, Rc<str>> {
    self.traces.borrow().clone()
  }

  pub fn get<T>(
    &self,
    promise_id: PromiseId,
    f: impl FnOnce(Option<&str>) -> T,
  ) -> T {
    f(self.traces.borrow().get(&promise_id).map(|x| x.as_ref()))
  }
}

#[derive(Clone)]
pub struct RuntimeActivityStatsFactory {
  pub(super) context_state: Rc<ContextState>,
  pub(super) op_state: Rc<RefCell<OpState>>,
}

/// Selects the statistics that you are interested in capturing.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct RuntimeActivityStatsFilter {
  include_timers: bool,
  include_ops: bool,
  include_resources: bool,
  op_filter: BitSet,
}

impl RuntimeActivityStatsFilter {
  pub fn all() -> Self {
    RuntimeActivityStatsFilter {
      include_ops: true,
      include_resources: true,
      include_timers: true,
      op_filter: BitSet::default(),
    }
  }

  pub fn with_ops(mut self) -> Self {
    self.include_ops = true;
    self
  }

  pub fn with_resources(mut self) -> Self {
    self.include_resources = true;
    self
  }

  pub fn with_timers(mut self) -> Self {
    self.include_timers = true;
    self
  }

  pub fn omit_op(mut self, op: OpId) -> Self {
    self.op_filter.insert(op as _);
    self
  }

  pub fn is_empty(&self) -> bool {
    // This ensures we don't miss a newly-added field in the empty comparison
    let Self {
      include_ops,
      include_resources,
      include_timers,
      op_filter: _,
    } = self;
    !(*include_ops) && !(*include_resources) && !(*include_timers)
  }
}

impl RuntimeActivityStatsFactory {
  /// Capture the current runtime activity.
  pub fn capture(
    self,
    filter: &RuntimeActivityStatsFilter,
  ) -> RuntimeActivityStats {
    let resources = if filter.include_resources {
      let res = &self.op_state.borrow().resource_table;
      let mut resources = ResourceOpenStats {
        resources: Vec::with_capacity(res.len()),
      };
      for resource in res.names() {
        resources
          .resources
          .push((resource.0, resource.1.to_string()))
      }
      resources
    } else {
      ResourceOpenStats::default()
    };

    let timers = if filter.include_timers {
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
      timers
    } else {
      TimerStats::default()
    };

    let (ops, opcall_traces) = if filter.include_ops {
      let ops = self.context_state.pending_ops.stats(&filter.op_filter);
      let opcall_traces = self.context_state.opcall_traces.capture();
      (ops, opcall_traces)
    } else {
      (OpInflightStats::default(), HashMap::default())
    };

    RuntimeActivityStats {
      context_state: self.context_state.clone(),
      ops,
      opcall_traces,
      resources,
      timers,
    }
  }
}

#[derive(Default)]
pub struct ResourceOpenStats {
  pub(super) resources: Vec<(u32, String)>,
}

#[derive(Default)]
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
  pub(super) ops: OpInflightStats,
  pub(super) opcall_traces: HashMap<PromiseId, Rc<str>>,
  pub(super) resources: ResourceOpenStats,
  pub(super) timers: TimerStats,
}

/// Contains an opcall stack trace.
#[derive(Debug, Clone)]
#[repr(transparent)]
pub struct OpCallTrace(Rc<str>);

impl Serialize for OpCallTrace {
  fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
  where
    S: Serializer,
  {
    self.0.as_ref().serialize(serializer)
  }
}

impl Deref for OpCallTrace {
  type Target = str;
  fn deref(&self) -> &Self::Target {
    self.0.as_ref()
  }
}

impl From<&Rc<str>> for OpCallTrace {
  fn from(value: &Rc<str>) -> Self {
    Self(value.clone())
  }
}

/// The type of runtime activity being tracked.
#[derive(Debug, Serialize)]
pub enum RuntimeActivity {
  /// An async op, including the promise ID and op name, with an optional opcall trace.
  AsyncOp(PromiseId, &'static str, Option<OpCallTrace>),
  /// A resource, including the resource ID and name.
  Resource(ResourceId, String),
  /// A timer, including the timer ID.
  Timer(usize),
  /// An interval, including the interval ID.
  Interval(usize),
}

impl RuntimeActivity {
  pub fn activity(&self) -> RuntimeActivityType {
    match self {
      Self::AsyncOp(..) => RuntimeActivityType::AsyncOp,
      Self::Resource(..) => RuntimeActivityType::Resource,
      Self::Timer(..) => RuntimeActivityType::Timer,
      Self::Interval(..) => RuntimeActivityType::Interval,
    }
  }
}

/// A data-less discriminant for [`RuntimeActivity`].
#[derive(
  Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize,
)]
pub enum RuntimeActivityType {
  AsyncOp,
  Resource,
  Timer,
  Interval,
}

impl RuntimeActivityStats {
  /// Capture the data within this [`RuntimeActivityStats`] as a [`RuntimeActivitySnapshot`]
  /// with details of activity.
  pub fn dump(&self) -> RuntimeActivitySnapshot {
    let has_traces = !self.opcall_traces.is_empty();
    let mut v = Vec::with_capacity(
      self.ops.ops.len()
        + self.resources.resources.len()
        + self.timers.timers.len(),
    );
    let ops = &self.context_state.op_ctxs;
    if has_traces {
      for op in self.ops.ops.iter() {
        v.push(RuntimeActivity::AsyncOp(
          op.0,
          ops[op.1 as usize].decl.name,
          self.opcall_traces.get(&op.0).map(|x| x.into()),
        ));
      }
    } else {
      for op in self.ops.ops.iter() {
        v.push(RuntimeActivity::AsyncOp(
          op.0,
          ops[op.1 as usize].decl.name,
          None,
        ));
      }
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
    let ops = &before.context_state.op_ctxs;

    let mut a = BitSet::new();
    for op in after.ops.ops.iter() {
      a.insert(op.0 as usize);
    }
    for op in before.ops.ops.iter() {
      if a.remove(op.0 as usize) {
        // continuing op
      } else {
        // before, but not after
        disappeared.push(RuntimeActivity::AsyncOp(
          op.0,
          ops[op.1 as usize].decl.name,
          before.opcall_traces.get(&op.0).map(|x| x.into()),
        ));
      }
    }
    for op in after.ops.ops.iter() {
      if a.contains(op.0 as usize) {
        // after but not before
        appeared.push(RuntimeActivity::AsyncOp(
          op.0,
          ops[op.1 as usize].decl.name,
          after.opcall_traces.get(&op.0).map(|x| x.into()),
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

#[derive(Debug, Serialize)]
pub struct RuntimeActivityDiff {
  pub appeared: Vec<RuntimeActivity>,
  pub disappeared: Vec<RuntimeActivity>,
}

#[derive(Debug, Serialize)]
pub struct RuntimeActivitySnapshot {
  pub active: Vec<RuntimeActivity>,
}
