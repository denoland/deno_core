| Rust                    | Fastcall | V8                            | 
|-------------------------|----------|-------------------------------|
| bool                    | X        | Bool                          |
| i8                      | X        | Uint32, Int32, Number, BigInt |
| u8                      | X        | Uint32, Int32, Number, BigInt |
| i16                     | X        | Uint32, Int32, Number, BigInt |
| u16                     | X        | Uint32, Int32, Number, BigInt |
| i32                     | X        | Uint32, Int32, Number, BigInt |
| u32                     | X        | Uint32, Int32, Number, BigInt |
| isize                   | X        | Uint32, Int32, Number, BigInt |
| usize                   | X        | Uint32, Int32, Number, BigInt |
#| f64                     | X        | Uint32, Int32, Number, BigInt |
#| f32                     | X        | Uint32, Int32, Number, BigInt |
| #[string] String        | X        | String                        |
| #[string] &str          | X        | String                        |
| v8::Local<v8::Value>    | X        | any                           |
| #[string] Cow<str>      | X        | String                        |
| &mut v8::Value          | X        | any                           |
| &v8::Value              | X        | any                           |
| &v8::String             | X        | String                        |
| v8::Local<v8::String>   | X        | String                        |
| v8::Local<v8::Object>   | X        | Object                        |
| &mut v8::String         | X        | String                        |
| &mut v8::Object         | X        | Object                        |
| &v8::Object             | X        | Object                        |
| #[serde] SerdeType      |          | any                           |
| #[serde] (Tuple, Tuple) |          | any                           |
| #[buffer] &mut [u8]     | X        | ArrayBuffer, ArrayBufferView  |
| #[buffer] &[u8]         | X        | ArrayBuffer, ArrayBufferView  |
