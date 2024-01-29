// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use std::any::Any;
use std::any::TypeId;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;

use deno_core::v8;

#[derive(Clone, Default)]
pub struct Output {
  pub lines: Arc<Mutex<Vec<String>>>,
}
impl Output {
  pub fn line(&self, line: String) {
    self.lines.lock().unwrap().push(line)
  }
}

#[derive(Default)]
pub struct TestFunctions {
  pub functions: Vec<(String, v8::Global<v8::Function>)>,
}

#[derive(Default)]
pub struct TestData {
  pub data: HashMap<(String, TypeId), Box<dyn Any>>,
}

impl TestData {
  pub fn insert<T: 'static + Any>(&mut self, name: String, data: T) {
    self.data.insert((name, TypeId::of::<T>()), Box::new(data));
  }

  pub fn get<T: 'static + Any>(&self, name: String) -> &T {
    let key = (name, TypeId::of::<T>());
    self
      .data
      .get(&key)
      .unwrap_or_else(|| {
        panic!(
          "Unable to locate {} of type {}",
          key.0,
          std::any::type_name::<T>()
        )
      })
      .downcast_ref()
      .unwrap()
  }

  pub fn take<T: 'static + Any>(&mut self, name: String) -> T {
    *self
      .data
      .remove(&(name, TypeId::of::<T>()))
      .unwrap()
      .downcast()
      .unwrap()
  }
}
