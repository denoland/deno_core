# op2

`#[op2]` is the in-progress replacement for `#[op]`.

# Parameters

<!-- START -->
| Rust | Fastcall | v8 |
|--|--|--|
| `bool` | ✅ | Bool |
| `i8` | ✅ | Uint32, Int32, Number, BigInt |
| `u8` | ✅ | Uint32, Int32, Number, BigInt |
| `i16` | ✅ | Uint32, Int32, Number, BigInt |
| `u16` | ✅ | Uint32, Int32, Number, BigInt |
| `i32` | ✅ | Uint32, Int32, Number, BigInt |
| `u32` | ✅ | Uint32, Int32, Number, BigInt |
| `isize` | ✅ | Uint32, Int32, Number, BigInt |
| `usize` | ✅ | Uint32, Int32, Number, BigInt |
| `#[string] String` | ✅ | String |
| `#[string] &str` | ✅ | String |
| `v8::Local<v8::Value>` | ✅ | any |
| `#[string] Cow<str>` | ✅ | String |
| `&mut v8::Value` | ✅ | any |
| `&v8::Value` | ✅ | any |
| `&v8::String` | ✅ | String |
| `v8::Local<v8::String>` | ✅ | String |
| `v8::Local<v8::Object>` | ✅ | Object |
| `&mut v8::String` | ✅ | String |
| `&mut v8::Object` | ✅ | Object |
| `&v8::Object` | ✅ | Object |
| `#[serde] SerdeType` |  | any |
| `#[serde] (Tuple, Tuple)` |  | any |
| `#[buffer] &mut [u8]` | ✅ | ArrayBuffer, ArrayBufferView |
| `#[buffer] &[u8]` | ✅ | ArrayBuffer, ArrayBufferView |
