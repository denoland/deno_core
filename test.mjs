const { DOMPointReadOnly, DOMPoint } = Deno.core.ops;

const point = new DOMPoint();
console.log(point instanceof DOMPointReadOnly); // true

console.log(point.subMethod()); // 3
