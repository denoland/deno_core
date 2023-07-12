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
Will always create a copy of the String data.
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
Will create a copy of the String data if it doesn't fit on the stack.
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
Will create a copy of the String data if it doesn't fit on the stack.
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
ArrayBuffer, ArrayBufferView (resizable=true,false)
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
ArrayBuffer, ArrayBufferView (resizable=true,false)
</td><td>
⚠️ JS may modify the contents of the slice if V8 is called re-entrantly.
</td></tr>
</table>