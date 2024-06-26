// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
use crate::magic::transl8::impl_magic;
use crate::magic::transl8::FromV8;
use crate::magic::transl8::ToV8;

/// serde_v8::Value is used internally to serialize/deserialize values in
/// objects and arrays. This struct was exposed to user code in the past, but
/// we don't want to do that anymore as it leads to inefficient usages - eg. wrapping
/// a V8 object in `serde_v8::Value` and then immediately unwrapping it.
//
// SAFETY: caveat emptor, the rust-compiler can no longer link lifetimes to their
// original scope, you must take special care in ensuring your handles don't
// outlive their scope.
pub struct GlobalValue {
  pub v8_value: v8::Global<v8::Value>,
}
impl_magic!(GlobalValue);

impl<'s, T> From<v8::Global<T>> for GlobalValue
where
  v8::Global<T>: Into<v8::Global<v8::Value>>,
{
  fn from(v: v8::Global<T>) -> Self {
    Self { v8_value: v.into() }
  }
}

impl From<GlobalValue> for v8::Global<v8::Value> {
  fn from(value: GlobalValue) -> Self {
    value.v8_value
  }
}

impl ToV8 for GlobalValue {
    fn to_v8<'a>(
        &self,
        scope: &mut v8::HandleScope<'a>,
      ) -> Result<v8::Local<'a, v8::Value>, crate::Error> {
        Ok(v8::Local::new(scope, self.v8_value.clone()))
    }
}

impl FromV8 for GlobalValue {
    fn from_v8(
        scope: &mut v8::HandleScope,
        value: v8::Local<v8::Value>,
      ) -> Result<Self, crate::Error> {
        Ok(Self {
            v8_value: v8::Global::new(scope, value),
        })
    }
}

mod test {
  #[test]
  fn magic_value() {
    use serde_v8_utilities::{js_exec, v8_do};
    struct Test(v8::Global<v8::Value>);
    impl<'de> serde::Deserialize<'de> for Test {
        fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
        where
            D: serde::Deserializer<'de>,
        {
            let value = super::GlobalValue::deserialize(deserializer)?;
            let local = value.v8_value;
            Ok(Self(local))
        }
    }

    v8_do(|| {
      // Init isolate
      let isolate = &mut v8::Isolate::new(v8::CreateParams::default());
      let handle_scope = &mut v8::HandleScope::new(isolate);
      let context = v8::Context::new(handle_scope);
      let scope = &mut v8::ContextScope::new(handle_scope, context);
      
      let v8_string = js_exec(scope, "'test'");
      let test: Test = crate::from_v8(scope, v8_string).expect("Could not deserialize");
      let local = v8::Local::new(scope, test.0);
      let test = local.to_rust_string_lossy(scope);
      assert_eq!(test.as_str(), "test");
    })
  }
}