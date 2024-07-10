# deno_ops

`proc_macro` for generating highly optimized V8 functions from Rust functions.

```rust,ignore
use deno_core::{op, extension};

// Declare an op.
#[op(fast)]
pub fn op_add(a: i32, b: i32) -> i32 {
  a + b
}

// Register with an extension.
extension!(
  math,
  ops = [op_add]
)
```
