import img from "./img.jpg" with { type: "bytes" };

console.log("Byte length", img.length);
console.log("Bytes", img.slice(0, 100));