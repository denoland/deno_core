# op2

`#[op2]` is the in-progress replacement for `#[op]`.

# Strings

`String`s in Rust are always UTF-8. `String`s in v8, however, are either two-byte UTF-16 or one-byte Latin-1. One-byte Latin-1 strings
are not byte-compatible with UTF-8, as characters with the index 128-255 require two bytes to encode in UTF-8.

Because of this, `String`s in `op`s always require a copy (at least) to ensure that we are not incorrectly passing Latin-1 data to
methods that expect a UTF-8 string. At this time there is no way to avoid this copy, though the `op` code does attempt to avoid
any allocations where possible by making use of a stack buffer.

# Parameters

<!-- START -->
<table><tr><th>Rust</th><th>Fastcall</th><th>v8</th></tr>
<tr>
<td>

```rust
bool
```

</td><td>
✅
</td><td>
Bool
</td><td>

</td></tr>
<tr>
<td>

```rust
i8
```

</td><td>
✅
</td><td>
Uint32, Int32, Number, BigInt
</td><td>

</td></tr>
<tr>
<td>

```rust
u8
```

</td><td>
✅
</td><td>
Uint32, Int32, Number, BigInt
</td><td>

</td></tr>
<tr>
<td>

```rust
i16
```

</td><td>
✅
</td><td>
Uint32, Int32, Number, BigInt
</td><td>

</td></tr>
<tr>
<td>

```rust
u16
```

</td><td>
✅
</td><td>
Uint32, Int32, Number, BigInt
</td><td>

</td></tr>
<tr>
<td>

```rust
i32
```

</td><td>
✅
</td><td>
Uint32, Int32, Number, BigInt
</td><td>

</td></tr>
<tr>
<td>

```rust
u32
```

</td><td>
✅
</td><td>
Uint32, Int32, Number, BigInt
</td><td>

</td></tr>
<tr>
<td>

```rust
isize
```

</td><td>
✅
</td><td>
Uint32, Int32, Number, BigInt
</td><td>

</td></tr>
<tr>
<td>

```rust
usize
```

</td><td>
✅
</td><td>
Uint32, Int32, Number, BigInt
</td><td>

</td></tr>
<tr>
<td>

```rust
#[string] String
```

</td><td>
✅
</td><td>
String
</td><td>
Fastcall available only if string is Latin-1. Will always create an allocated, UTF-8 copy of the String data.
</td></tr>
<tr>
<td>

```rust
#[string] &str
```

</td><td>
✅
</td><td>
String
</td><td>
Fastcall available only if string is Latin-1. Will create an owned `String` copy of the String data if it doesn't fit on the stack. Will never allocate in a fastcall, but will copy Latin-1 -> UTF-8.
</td></tr>
<tr>
<td>

```rust
#[string] Cow<str>
```

</td><td>
✅
</td><td>
String
</td><td>
Fastcall available only if string is Latin-1. Will create a `Cow::Owned` copy of the String data if it doesn't fit on the stack. Will always be `Cow::Borrowed` in a fastcall, but will copy Latin-1 -> UTF-8.
</td></tr>
<tr>
<td>

```rust
&v8::Value
```

</td><td>
✅
</td><td>
any
</td><td>

</td></tr>
<tr>
<td>

```rust
&v8::String
```

</td><td>
✅
</td><td>
String
</td><td>

</td></tr>
<tr>
<td>

```rust
&v8::Object
```

</td><td>
✅
</td><td>
Object
</td><td>

</td></tr>
<tr>
<td>

```rust
&v8::Function
```

</td><td>
✅
</td><td>
Function
</td><td>

</td></tr>
<tr>
<td>

```rust
&v8::...
```

</td><td>
✅
</td><td>
...
</td><td>

</td></tr>
<tr>
<td>

```rust
&mut v8::Value
```

</td><td>
✅
</td><td>
any
</td><td>

</td></tr>
<tr>
<td>

```rust
&mut v8::String
```

</td><td>
✅
</td><td>
String
</td><td>

</td></tr>
<tr>
<td>

```rust
&mut v8::Object
```

</td><td>
✅
</td><td>
Object
</td><td>

</td></tr>
<tr>
<td>

```rust
&mut v8::Function
```

</td><td>
✅
</td><td>
Function
</td><td>

</td></tr>
<tr>
<td>

```rust
&mut v8::...
```

</td><td>
✅
</td><td>
...
</td><td>

</td></tr>
<tr>
<td>

```rust
v8::Local<v8::Value>
```

</td><td>
✅
</td><td>
any
</td><td>

</td></tr>
<tr>
<td>

```rust
v8::Local<v8::String>
```

</td><td>
✅
</td><td>
String
</td><td>

</td></tr>
<tr>
<td>

```rust
v8::Local<v8::Object>
```

</td><td>
✅
</td><td>
Object
</td><td>

</td></tr>
<tr>
<td>

```rust
v8::Local<v8::Function>
```

</td><td>
✅
</td><td>
Function
</td><td>

</td></tr>
<tr>
<td>

```rust
v8::Local<v8::...>
```

</td><td>
✅
</td><td>
...
</td><td>

</td></tr>
<tr>
<td>

```rust
#[serde] SerdeType
```

</td><td>

</td><td>
any
</td><td>
⚠️ May be slow.
</td></tr>
<tr>
<td>

```rust
#[serde] (Tuple, Tuple)
```

</td><td>

</td><td>
any
</td><td>
⚠️ May be slow.
</td></tr>
<tr>
<td>

```rust
#[buffer] &mut [u8]
```

</td><td>
✅
</td><td>
UInt8Array (resizable=true,false)
</td><td>
⚠️ JS may modify the contents of the slice if V8 is called re-entrantly.
</td></tr>
<tr>
<td>

```rust
#[buffer] &[u8]
```

</td><td>
✅
</td><td>
UInt8Array (resizable=true,false)
</td><td>
⚠️ JS may modify the contents of the slice if V8 is called re-entrantly.
</td></tr>
<tr>
<td>

```rust
#[buffer(copy)] Vec<u8>
```

</td><td>
✅
</td><td>
ArrayBuffer, ArrayBufferView, SharedArrayBuffer
</td><td>
Safe, but forces a copy.
</td></tr>
<tr>
<td>

```rust
#[buffer(copy)] Box<[u8]>
```

</td><td>
✅
</td><td>
ArrayBuffer, ArrayBufferView, SharedArrayBuffer
</td><td>
Safe, but forces a copy.
</td></tr>
<tr>
<td>

```rust
#[buffer(copy)] bytes::Bytes
```

</td><td>
✅
</td><td>
ArrayBuffer, ArrayBufferView, SharedArrayBuffer
</td><td>
Safe, but forces a copy.
</td></tr>
<tr>
<td>

```rust
&OpState
```

</td><td>
✅
</td><td>

</td><td>

</td></tr>
<tr>
<td>

```rust
&mut OpState
```

</td><td>
✅
</td><td>

</td><td>

</td></tr>
<tr>
<td>

```rust
Rc<RefCell<OpState>>
```

</td><td>
✅
</td><td>

</td><td>

</td></tr>
<tr>
<td>

```rust
#[state] &StateObject
```

</td><td>
✅
</td><td>

</td><td>
Extracts an object from `OpState`.
</td></tr>
<tr>
<td>

```rust
#[state] &mut StateObject
```

</td><td>
✅
</td><td>

</td><td>
Extracts an object from `OpState`.
</td></tr>
</table>