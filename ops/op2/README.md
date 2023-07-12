# op2

`#[op2]` is the in-progress replacement for `#[op]`.

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
</tr>
<tr>
<td>

```rust
i8
```

</td><td>
✅
</td><td>
Uint32, Int32, Number, BigInt
</tr>
<tr>
<td>

```rust
u8
```

</td><td>
✅
</td><td>
Uint32, Int32, Number, BigInt
</tr>
<tr>
<td>

```rust
i16
```

</td><td>
✅
</td><td>
Uint32, Int32, Number, BigInt
</tr>
<tr>
<td>

```rust
u16
```

</td><td>
✅
</td><td>
Uint32, Int32, Number, BigInt
</tr>
<tr>
<td>

```rust
i32
```

</td><td>
✅
</td><td>
Uint32, Int32, Number, BigInt
</tr>
<tr>
<td>

```rust
u32
```

</td><td>
✅
</td><td>
Uint32, Int32, Number, BigInt
</tr>
<tr>
<td>

```rust
isize
```

</td><td>
✅
</td><td>
Uint32, Int32, Number, BigInt
</tr>
<tr>
<td>

```rust
usize
```

</td><td>
✅
</td><td>
Uint32, Int32, Number, BigInt
</tr>
<tr>
<td>

```rust
#[string] String
```

</td><td>
✅
</td><td>
String
</tr>
<tr>
<td>

```rust
#[string] &str
```

</td><td>
✅
</td><td>
String
</tr>
<tr>
<td>

```rust
v8::Local<v8::Value>
```

</td><td>
✅
</td><td>
any
</tr>
<tr>
<td>

```rust
#[string] Cow<str>
```

</td><td>
✅
</td><td>
String
</tr>
<tr>
<td>

```rust
&mut v8::Value
```

</td><td>
✅
</td><td>
any
</tr>
<tr>
<td>

```rust
&v8::Value
```

</td><td>
✅
</td><td>
any
</tr>
<tr>
<td>

```rust
&v8::String
```

</td><td>
✅
</td><td>
String
</tr>
<tr>
<td>

```rust
v8::Local<v8::String>
```

</td><td>
✅
</td><td>
String
</tr>
<tr>
<td>

```rust
v8::Local<v8::Object>
```

</td><td>
✅
</td><td>
Object
</tr>
<tr>
<td>

```rust
&mut v8::String
```

</td><td>
✅
</td><td>
String
</tr>
<tr>
<td>

```rust
&mut v8::Object
```

</td><td>
✅
</td><td>
Object
</tr>
<tr>
<td>

```rust
&v8::Object
```

</td><td>
✅
</td><td>
Object
</tr>
<tr>
<td>

```rust
#[serde] SerdeType
```

</td><td>

</td><td>
any
</tr>
<tr>
<td>

```rust
#[serde] (Tuple, Tuple)
```

</td><td>

</td><td>
any
</tr>
<tr>
<td>

```rust
#[buffer] &mut [u8]
```

</td><td>
✅
</td><td>
ArrayBuffer, ArrayBufferView
</tr>
<tr>
<td>

```rust
#[buffer] &[u8]
```

</td><td>
✅
</td><td>
ArrayBuffer, ArrayBufferView
</tr>
</table>