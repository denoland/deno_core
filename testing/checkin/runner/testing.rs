// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
use std::any::Any;
use std::any::TypeId;
use std::collections::HashMap;

use deno_core::v8;

#[derive(Default)]
pub struct Output {
  pub lines: Vec<String>,
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
    self
      .data
      .get(&(name, TypeId::of::<T>()))
      .unwrap()
      .downcast_ref()
      .unwrap()
  }
}
