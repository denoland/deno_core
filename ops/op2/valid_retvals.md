| Supported | Rust                             | Fastcall | V8     | Notes |
| --------- | -------------------------------- | -------- | ------ | ----- |
| X         | bool                             | X        | Bool   |       |
| X         | i8                               | X        | Int32  |       |
| X         | u8                               | X        | Uint32 |       |
| X         | i16                              | X        | Int32  |       |
| X         | u16                              | X        | Uint32 |       |
| X         | i32                              | X        | Int32  |       |
| X         | u32                              | X        | Uint32 |       |
| X         | #[bigint] i64                    |          | BigInt |       |
| X         | #[bigint] u64                    |          | BigInt |       |
| X         | #[bigint] isize                  |          | BigInt |       |
| X         | #[bigint] usize                  |          | BigInt |       |
| X         | f32                              | X        | Number |       |
| X         | f64                              | X        | Number |       |
| X         | #[string] String                 |          | String |       |
|           | #[string] &str                   |          | String |       |
|           | #[string] Cow<str>               |          | String |       |
| X         | v8::Local<v8::Value>             |          | any    |       |
| X         | v8::Local<v8::**V8**>            |          | **V8** |       |
|           | #[global] v8::Global<v8::Value>  |          | any    |       |
|           | #[global] v8::Global<v8::**V8**> |          | **V8** |       |
| X         | #[serde] SerdeType               |          | any    |       |
| X         | #[serde] (Tuple, Tuple)          |          | any    |       |
