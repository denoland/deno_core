import img from "./img.jpg" with { type: "bytes" };
import textBytes from "./text.txt" with { type: "bytes" };
import text from "./text.txt" with { type: "text" };

console.log("Byte length", img.length);
console.log("Bytes", img.slice(0, 100));
console.log("Byte length", textBytes.length);
console.log("Bytes", textBytes.slice(0, 100));
console.log("Text length", text.length);
console.log("Text", text.slice(0, 100));