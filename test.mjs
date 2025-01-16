const { DOMPointReadOnly, DOMPoint } = Deno.core.ops;

const point = new DOMPoint();
console.log(point instanceof DOMPointReadOnly); // true

point.subMethod();

console.log(point.x);
