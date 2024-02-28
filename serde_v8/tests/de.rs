// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
use serde::Deserialize;
use serde::Deserializer;

use serde_v8::BigInt;
use serde_v8::ByteString;
use serde_v8::Error;
use serde_v8::JsBuffer;
use serde_v8::U16String;
use serde_v8_utilities::js_exec;
use serde_v8_utilities::v8_do;

#[derive(Debug, Deserialize, PartialEq)]
struct MathOp {
  pub a: u64,
  pub b: u64,
  pub operator: Option<String>,
}

#[derive(Debug, PartialEq, Deserialize)]
enum EnumUnit {
  A,
  B,
  C,
}

#[derive(Debug, PartialEq, Deserialize)]
enum EnumPayloads {
  UInt(u64),
  Int(i64),
  Float(f64),
  Point { x: i64, y: i64 },
  Tuple(bool, i64, ()),
}

fn dedo(
  code: &str,
  f: impl FnOnce(&mut v8::HandleScope, v8::Local<v8::Value>),
) {
  v8_do(|| {
    let isolate = &mut v8::Isolate::new(v8::CreateParams::default());
    let handle_scope = &mut v8::HandleScope::new(isolate);
    let context = v8::Context::new(handle_scope);
    let scope = &mut v8::ContextScope::new(handle_scope, context);
    let v = js_exec(scope, code);

    f(scope, v);
  })
}

macro_rules! decheck {
  ($fn_name:ident, $t:ty, $src:expr, $x:ident, $check:expr) => {
    #[test]
    fn $fn_name() {
      #[allow(clippy::bool_assert_comparison)]
      dedo($src, |scope, v| {
        let rt = serde_v8::from_v8(scope, v);
        assert!(rt.is_ok(), "from_v8(\"{}\"): {:?}", $src, rt.err());
        let $x: $t = rt.unwrap();
        $check
      });
    }
  };
}

macro_rules! detest {
  ($fn_name:ident, $t:ty, $src:expr, $rust:expr) => {
    decheck!($fn_name, $t, $src, t, assert_eq!(t, $rust));
  };
}

macro_rules! defail {
  ($fn_name:ident, $t:ty, $src:expr, $failcase:expr) => {
    #[test]
    fn $fn_name() {
      #[allow(clippy::bool_assert_comparison, clippy::redundant_closure_call)]
      dedo($src, |scope, v| {
        let rt: serde_v8::Result<$t> = serde_v8::from_v8(scope, v);
        let rtstr = format!("{:?}", rt);
        let failed_as_expected = $failcase(rt);
        assert!(
          failed_as_expected,
          "expected failure on deserialize(\"{}\"), got: {}",
          $src, rtstr
        );
      });
    }
  };
}

detest!(de_option_some, Option<bool>, "true", Some(true));
detest!(de_option_null, Option<bool>, "null", None);
detest!(de_option_undefined, Option<bool>, "undefined", None);
detest!(de_unit_null, (), "null", ());
detest!(de_unit_undefined, (), "undefined", ());
detest!(de_bool, bool, "true", true);
detest!(de_char, char, "'é'", 'é');
detest!(de_u64, u64, "32", 32);
detest!(de_string, String, "'Hello'", "Hello".to_owned());
detest!(de_vec_empty, Vec<u64>, "[]", vec![0; 0]);
detest!(de_vec_u64, Vec<u64>, "[1,2,3,4,5]", vec![1, 2, 3, 4, 5]);
detest!(
  de_vec_str,
  Vec<String>,
  "['hello', 'world']",
  vec!["hello".to_owned(), "world".to_owned()]
);
detest!(
  de_tuple,
  (u64, bool, ()),
  "[123, true, null]",
  (123, true, ())
);
defail!(
  de_tuple_wrong_len_short,
  (u64, bool, ()),
  "[123, true]",
  |e| e == Err(Error::LengthMismatch(2, 3))
);
defail!(
  de_tuple_wrong_len_long,
  (u64, bool, ()),
  "[123, true, null, 'extra']",
  |e| e == Err(Error::LengthMismatch(4, 3))
);
detest!(
  de_mathop,
  MathOp,
  "({a: 1, b: 3, c: 'ignored'})",
  MathOp {
    a: 1,
    b: 3,
    operator: None
  }
);

// Unit enums
detest!(de_enum_unit_a, EnumUnit, "'A'", EnumUnit::A);
detest!(de_enum_unit_so_a, EnumUnit, "new String('A')", EnumUnit::A);
detest!(de_enum_unit_b, EnumUnit, "'B'", EnumUnit::B);
detest!(de_enum_unit_so_b, EnumUnit, "new String('B')", EnumUnit::B);
detest!(de_enum_unit_c, EnumUnit, "'C'", EnumUnit::C);
detest!(de_enum_unit_so_c, EnumUnit, "new String('C')", EnumUnit::C);

// Enums with payloads (tuples & struct)
detest!(
  de_enum_payload_int,
  EnumPayloads,
  "({ Int: -123 })",
  EnumPayloads::Int(-123)
);
detest!(
  de_enum_payload_uint,
  EnumPayloads,
  "({ UInt: 123 })",
  EnumPayloads::UInt(123)
);
detest!(
  de_enum_payload_float,
  EnumPayloads,
  "({ Float: 1.23 })",
  EnumPayloads::Float(1.23)
);
detest!(
  de_enum_payload_point,
  EnumPayloads,
  "({ Point: { x: 1, y: 2 } })",
  EnumPayloads::Point { x: 1, y: 2 }
);
detest!(
  de_enum_payload_tuple,
  EnumPayloads,
  "({ Tuple: [true, 123, null ] })",
  EnumPayloads::Tuple(true, 123, ())
);

#[test]
fn de_f64() {
  dedo("12345.0", |scope, v| {
    let x: f64 = serde_v8::from_v8(scope, v).unwrap();
    assert!((x - 12345.0).abs() < f64::EPSILON);
  });
}

#[test]
fn de_map() {
  use std::collections::HashMap;

  dedo("({a: 1, b: 2, c: 3})", |scope, v| {
    let map: HashMap<String, u64> = serde_v8::from_v8(scope, v).unwrap();
    assert_eq!(map.get("a").cloned(), Some(1));
    assert_eq!(map.get("b").cloned(), Some(2));
    assert_eq!(map.get("c").cloned(), Some(3));
    assert_eq!(map.get("nada"), None);
  })
}

#[test]
fn de_obj_with_numeric_keys() {
  dedo(
    r#"({
  lines: {
    100: {
      unit: "m"
    },
    200: {
      unit: "cm"
    }
  }
})"#,
    |scope, v| {
      let json: serde_json::Value = serde_v8::from_v8(scope, v).unwrap();
      assert_eq!(
        json.to_string(),
        r#"{"lines":{"100":{"unit":"m"},"200":{"unit":"cm"}}}"#
      );
    },
  )
}

#[test]
fn de_string_or_buffer() {
  dedo("'hello'", |scope, v| {
    let sob: serde_v8::StringOrBuffer = serde_v8::from_v8(scope, v).unwrap();
    assert_eq!(sob.as_ref(), &[0x68, 0x65, 0x6C, 0x6C, 0x6F]);
  });

  dedo("new Uint8Array([97])", |scope, v| {
    let sob: serde_v8::StringOrBuffer = serde_v8::from_v8(scope, v).unwrap();
    assert_eq!(sob.as_ref(), &[97]);
  });

  dedo("new Uint8Array([128])", |scope, v| {
    let sob: serde_v8::StringOrBuffer = serde_v8::from_v8(scope, v).unwrap();
    assert_eq!(sob.as_ref(), &[128]);
  });

  dedo(
    "(Uint8Array.from([0x68, 0x65, 0x6C, 0x6C, 0x6F]))",
    |scope, v| {
      let sob: serde_v8::StringOrBuffer = serde_v8::from_v8(scope, v).unwrap();
      assert_eq!(sob.as_ref(), &[0x68, 0x65, 0x6C, 0x6C, 0x6F]);
    },
  );
}

#[test]
fn de_buffers() {
  // ArrayBufferView
  dedo("new Uint8Array([97])", |scope, v| {
    let buf: JsBuffer = serde_v8::from_v8(scope, v).unwrap();
    assert_eq!(&*buf, &[97]);
  });

  // ArrayBuffer
  dedo("(new Uint8Array([97])).buffer", |scope, v| {
    let buf: JsBuffer = serde_v8::from_v8(scope, v).unwrap();
    assert_eq!(&*buf, &[97]);
  });

  dedo(
    "(Uint8Array.from([0x68, 0x65, 0x6C, 0x6C, 0x6F]))",
    |scope, v| {
      let buf: JsBuffer = serde_v8::from_v8(scope, v).unwrap();
      assert_eq!(&*buf, &[0x68, 0x65, 0x6C, 0x6C, 0x6F]);
    },
  );

  dedo("(new ArrayBuffer(4))", |scope, v| {
    let buf: JsBuffer = serde_v8::from_v8(scope, v).unwrap();
    assert_eq!(&*buf, &[0x0, 0x0, 0x0, 0x0]);
  });

  dedo("(new ArrayBuffer(8, { maxByteLength: 16}))", |scope, v| {
    let result: Result<JsBuffer, Error> = serde_v8::from_v8(scope, v);
    matches!(result, Err(Error::ResizableBackingStoreNotSupported));
  });
}

// Structs
#[derive(Debug, PartialEq, Deserialize)]
struct StructUnit;

#[derive(Debug, PartialEq)]
struct StructPayload {
  a: u64,
  b: u64,
}

struct StructVisitor;

impl<'de> serde::de::Visitor<'de> for StructVisitor {
  type Value = StructPayload;
  fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
    formatter.write_str("struct StructPayload")
  }
  fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
  where
    A: serde::de::MapAccess<'de>,
  {
    let mut payload = StructPayload { a: 0, b: 0 };
    while let Some(key) = map.next_key::<String>()? {
      match key.as_ref() {
        "a" => payload.a = map.next_value()?,
        "b" => payload.b = map.next_value()?,
        f => panic!("Unknown field {f}"),
      }
    }
    Ok(payload)
  }
}

detest!(de_unit_struct, StructUnit, "'StructUnit'", StructUnit);

#[test]
fn de_struct() {
  dedo("({ a: 1, b: 2 })", |scope, v| {
    let mut de = serde_v8::Deserializer::new(scope, v, None);
    let payload = de
      .deserialize_struct("StructPayload", &[], StructVisitor)
      .unwrap();
    assert_eq!(payload, StructPayload { a: 1, b: 2 })
  })
}

#[test]
fn de_struct_hint() {
  dedo("({ a: 1, b: 2 })", |scope, v| {
    let mut de = serde_v8::Deserializer::new(scope, v, None);
    let payload = de
      .deserialize_struct("StructPayload", &["a", "b"], StructVisitor)
      .unwrap();
    assert_eq!(payload, StructPayload { a: 1, b: 2 })
  })
}

////
// JSON tests: serde_json::Value compatibility
////

detest!(
  de_json_null,
  serde_json::Value,
  "null",
  serde_json::Value::Null
);
detest!(
  de_json_bool,
  serde_json::Value,
  "true",
  serde_json::Value::Bool(true)
);
detest!(
  de_json_int,
  serde_json::Value,
  "123",
  serde_json::Value::Number(serde_json::Number::from(123))
);
detest!(
  de_json_float,
  serde_json::Value,
  "123.45",
  serde_json::Value::Number(serde_json::Number::from_f64(123.45).unwrap())
);
detest!(
  de_json_string,
  serde_json::Value,
  "'Hello'",
  serde_json::Value::String("Hello".to_string())
);
detest!(
  de_json_vec_string,
  serde_json::Value,
  "['Hello', 'World']",
  serde_json::Value::Array(vec![
    serde_json::Value::String("Hello".to_string()),
    serde_json::Value::String("World".to_string())
  ])
);
detest!(
  de_json_tuple,
  serde_json::Value,
  "[true, 'World', 123.45, null]",
  serde_json::Value::Array(vec![
    serde_json::Value::Bool(true),
    serde_json::Value::String("World".to_string()),
    serde_json::Value::Number(serde_json::Number::from_f64(123.45).unwrap()),
    serde_json::Value::Null,
  ])
);
detest!(
  de_json_object,
  serde_json::Value,
  "({a: 1, b: 'hello', c: true})",
  serde_json::json!({
    "a": 1,
    "b": "hello",
    "c": true,
  })
);
detest!(
  de_json_object_from_map,
  serde_json::Value,
  "(new Map([['a', 1], ['b', 'hello'], ['c', true]]))",
  serde_json::json!({
    "a": 1,
    "b": "hello",
    "c": true,
  })
);
// TODO: this is not optimal, ideally we'd get an array of [1,2,3] instead.
// Fixing that will require exposing Set::AsArray in the v8 bindings.
detest!(
  de_json_object_from_set,
  serde_json::Value,
  "(new Set([1, 2, 3]))",
  serde_json::json!({})
);

defail!(defail_struct, MathOp, "123", |e| e
  == Err(Error::ExpectedObject("Number")));

#[derive(Eq, PartialEq, Debug, Deserialize)]
pub struct SomeThing {
  pub a: String,
  #[serde(default)]
  pub b: String,
}
detest!(
  de_struct_defaults,
  SomeThing,
  "({ a: 'hello' })",
  SomeThing {
    a: "hello".into(),
    b: "".into()
  }
);

detest!(de_bstr, ByteString, "'hello'", "hello".into());
defail!(defail_bstr, ByteString, "'👋bye'", |e| e
  == Err(Error::ExpectedLatin1));

#[derive(Eq, PartialEq, Debug, Deserialize)]
pub struct StructWithBytes {
  #[serde(with = "serde_bytes")]
  a: Vec<u8>,
  #[serde(with = "serde_bytes")]
  b: Vec<u8>,
  #[serde(with = "serde_bytes")]
  c: Vec<u8>,
}
detest!(
  de_struct_with_bytes,
  StructWithBytes,
  "({ a: new Uint8Array([1, 2]), b: (new Uint8Array([3 , 4])).buffer, c: (new Uint32Array([0])).buffer})",
  StructWithBytes {
    a: vec![1, 2],
    b: vec![3, 4],
    c: vec![0, 0, 0, 0],
  }
);
detest!(
  de_u16str,
  U16String,
  "'hello'",
  "hello".encode_utf16().collect::<Vec<_>>().into()
);
detest!(
  de_u16str_non_latin1,
  U16String,
  "'👋bye'",
  "👋bye".encode_utf16().collect::<Vec<_>>().into()
);

// NaN
detest!(de_nan_u8, u8, "NaN", 0);
detest!(de_nan_u16, u16, "NaN", 0);
detest!(de_nan_u32, u32, "NaN", 0);
detest!(de_nan_u64, u64, "NaN", 0);
detest!(de_nan_i8, i8, "NaN", 0);
detest!(de_nan_i16, i16, "NaN", 0);
detest!(de_nan_i32, i32, "NaN", 0);
detest!(de_nan_i64, i64, "NaN", 0);
decheck!(de_nan_f32, f32, "NaN", t, assert!(t.is_nan()));
decheck!(de_nan_f64, f64, "NaN", t, assert!(t.is_nan()));

// Infinity
detest!(de_inf_u8, u8, "Infinity", u8::MAX);
detest!(de_inf_u16, u16, "Infinity", u16::MAX);
detest!(de_inf_u32, u32, "Infinity", u32::MAX);
detest!(de_inf_u64, u64, "Infinity", u64::MAX);
detest!(de_inf_i8, i8, "Infinity", i8::MAX);
detest!(de_inf_i16, i16, "Infinity", i16::MAX);
detest!(de_inf_i32, i32, "Infinity", i32::MAX);
detest!(de_inf_i64, i64, "Infinity", i64::MAX);
detest!(de_inf_f32, f32, "Infinity", f32::INFINITY);
detest!(de_inf_f64, f64, "Infinity", f64::INFINITY);

// -Infinity
detest!(de_neg_inf_u8, u8, "-Infinity", u8::MIN);
detest!(de_neg_inf_u16, u16, "-Infinity", u16::MIN);
detest!(de_neg_inf_u32, u32, "-Infinity", u32::MIN);
detest!(de_neg_inf_u64, u64, "-Infinity", u64::MIN);
detest!(de_neg_inf_i8, i8, "-Infinity", i8::MIN);
detest!(de_neg_inf_i16, i16, "-Infinity", i16::MIN);
detest!(de_neg_inf_i32, i32, "-Infinity", i32::MIN);
detest!(de_neg_inf_i64, i64, "-Infinity", i64::MIN);
detest!(de_neg_inf_f32, f32, "-Infinity", f32::NEG_INFINITY);
detest!(de_neg_inf_f64, f64, "-Infinity", f64::NEG_INFINITY);

// BigInt to f32/f64 max/min
detest!(
  de_bigint_f64_max,
  f64,
  "BigInt(1.7976931348623157e+308)",
  f64::MAX
);
detest!(
  de_bigint_f64_min,
  f64,
  "BigInt(-1.7976931348623157e+308)",
  f64::MIN
);
detest!(de_bigint_f32_max, f32, "BigInt(3.40282347e38)", f32::MAX);
detest!(de_bigint_f32_min, f32, "BigInt(-3.40282347e38)", f32::MIN);
// BigInt to f32/f64 saturating to inf
detest!(
  de_bigint_f64_inf,
  f64,
  "(BigInt(1.7976931348623157e+308)*BigInt(100))",
  f64::INFINITY
);
detest!(
  de_bigint_f64_neg_inf,
  f64,
  "(BigInt(-1.7976931348623157e+308)*BigInt(100))",
  f64::NEG_INFINITY
);

detest!(
  de_bigint_f32_inf,
  f32,
  "BigInt(1.7976931348623157e+308)",
  f32::INFINITY
);
detest!(
  de_bigint_f32_neg_inf,
  f32,
  "BigInt(-1.7976931348623157e+308)",
  f32::NEG_INFINITY
);

// BigInt to BigInt
detest!(
  de_bigint_var_u8,
  BigInt,
  "255n",
  num_bigint::BigInt::from(255u8).into()
);
detest!(
  de_bigint_var_i8,
  BigInt,
  "-128n",
  num_bigint::BigInt::from(-128i8).into()
);
detest!(
  de_bigint_var_u16,
  BigInt,
  "65535n",
  num_bigint::BigInt::from(65535u16).into()
);
detest!(
  de_bigint_var_i16,
  BigInt,
  "-32768n",
  num_bigint::BigInt::from(-32768i16).into()
);
detest!(
  de_bigint_var_u32,
  BigInt,
  "4294967295n",
  num_bigint::BigInt::from(4294967295u32).into()
);
detest!(
  de_bigint_var_i32,
  BigInt,
  "-2147483648n",
  num_bigint::BigInt::from(-2147483648i32).into()
);
detest!(
  de_bigint_var_u64,
  BigInt,
  "18446744073709551615n",
  num_bigint::BigInt::from(18446744073709551615u64).into()
);
detest!(
  de_bigint_var_i64,
  BigInt,
  "-9223372036854775808n",
  num_bigint::BigInt::from(-9223372036854775808i64).into()
);
detest!(
  de_bigint_var_u128,
  BigInt,
  "340282366920938463463374607431768211455n",
  num_bigint::BigInt::from(340282366920938463463374607431768211455u128).into()
);
detest!(
  de_bigint_var_i128,
  BigInt,
  "-170141183460469231731687303715884105728n",
  num_bigint::BigInt::from(-170141183460469231731687303715884105728i128).into()
);
