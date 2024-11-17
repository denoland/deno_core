const { DOMPoint } = Deno.core.ops;

const point = new DOMPoint(222, 2, 3, 4);

console.log(point.x);
point.x = 5;
console.log(point.x);
