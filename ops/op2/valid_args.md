| Supported | Rust                           | Fastcall | V8                                                  | Notes                                                                   |
| --------- | ------------------------------ | -------- | --------------------------------------------------- | ----------------------------------------------------------------------- |
| X         | bool                           | X        | Bool                                                |                                                                         |
| X         | i8                             | X        | Uint32, Int32, Number, BigInt                       |                                                                         |
| X         | u8                             | X        | Uint32, Int32, Number, BigInt                       |                                                                         |
| X         | i16                            | X        | Uint32, Int32, Number, BigInt                       |                                                                         |
| X         | u16                            | X        | Uint32, Int32, Number, BigInt                       |                                                                         |
| X         | i32                            | X        | Uint32, Int32, Number, BigInt                       |                                                                         |
| X         | u32                            | X        | Uint32, Int32, Number, BigInt                       |                                                                         |
| X         | isize                          | X        | Uint32, Int32, Number, BigInt                       |                                                                         |
| X         | usize                          | X        | Uint32, Int32, Number, BigInt                       |                                                                         |
|           | f32                            | X        | Uint32, Int32, Number, BigInt                       |                                                                         |
|           | f64                            | X        | Uint32, Int32, Number, BigInt                       |                                                                         |
| X         | #[string] String               | X        | String                                              | Will always create a copy of the String data.                           |
| X         | #[string] &str                 | X        | String                                              | Will create a copy of the String data if it doesn't fit on the stack.   |
| X         | #[string] Cow<str>             | X        | String                                              | Will create a copy of the String data if it doesn't fit on the stack.   |
| X         | &v8::Value                     | X        | any                                                 |                                                                         |
| X         | &v8::**V8**                    | X        | **V8**                                              |                                                                         |
| X         | &mut v8::Value                 | X        | any                                                 |                                                                         |
| X         | &mut v8::**V8**                | X        | **V8**                                              |                                                                         |
| X         | v8::Local<v8::Value>           | X        | any                                                 |                                                                         |
| X         | v8::Local<v8::**V8**>          | X        | **V8**                                              |                                                                         |
| X         | #[serde] SerdeType             |          | any                                                 | ⚠️ May be slow.                                                          |
| X         | #[serde] (Tuple, Tuple)        |          | any                                                 | ⚠️ May be slow.                                                          |
| X         | #[buffer] &mut [u8]            | X        | ArrayBuffer, ArrayBufferView (resizable=true,false) | ⚠️ JS may modify the contents of the slice if V8 is called re-entrantly. |
| X         | #[buffer] &[u8]                | X        | ArrayBuffer, ArrayBufferView (resizable=true,false) | ⚠️ JS may modify the contents of the slice if V8 is called re-entrantly. |
|           | #[buffer] V8Slice              | X        | ArrayBuffer, ArrayBufferView (resizable=false)      | ⚠️ JS may modify the contents of slices obtained from buffer.            |
|           | #[buffer(detach)] V8Slice      | X        | ArrayBuffer, ArrayBufferView (resizable=true,false) | Safe.                                                                   |
|           | #[buffer] V8ResizableSlice     | X        | ArrayBuffer, ArrayBufferView (resizable=true)       | ⚠️ JS may modify the contents of slices obtained from buffer.            |
|           | #[buffer] JSBuffer             | X        | ArrayBuffer, ArrayBufferView (resizable=false)      | ⚠️ JS may modify the contents of slices obtained from buffer.            |
|           | #[buffer(detach)] JSBuffer     | X        | ArrayBuffer, ArrayBufferView (resizable=true,false) | Safe.                                                                   |
|           | #[buffer(copy)] Vec<u8>        | X        | ArrayBuffer, ArrayBufferView, SharedArrayBuffer     | Safe.                                                                   |
|           | #[buffer(copy)] Box<[u8]>      | X        | ArrayBuffer, ArrayBufferView, SharedArrayBuffer     | Safe.                                                                   |
|           | #[buffer(unsafe)] bytes::Bytes | X        | ArrayBuffer, ArrayBufferView (resizable=false)      | ⚠️ JS may modify the contents of the buffer.                             |
|           | #[buffer(detach)] bytes::Bytes | X        | ArrayBuffer, ArrayBufferView (resizable=true,false) | Safe.                                                                   |
|           | #[buffer(copy)] bytes::Bytes   | X        | ArrayBuffer, ArrayBufferView, SharedArrayBuffer     | Safe.                                                                   |
