// This test refers to the issue.
// https://github.com/denoland/deno_core/issues/744

// Using `Object.prototype.__defineSetter__()`,
// When converting from `serde_v8::from_v8` to `Vec<JsStackFrame>`,
// Panic is caused by `unwrap_failed`.
Object.prototype.__defineSetter__(0, function () {});

// Test that JavaScript syntax error statements are output with no panic
// For example, `ReferenceError: variable is not defined`.
variable;
