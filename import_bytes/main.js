import img from "./img.jpg" with { type: "bytes" };
import imgUrl from "./img.jpg" with { type: "url" };
import textBytes from "./text.txt" with { type: "bytes" };
import text from "./text.txt" with { type: "text" };
import textUrl from "./text.txt" with { type: "url" };

console.log("Img byte length", img.length);
console.log("Img bytes", img.slice(0, 5));
console.log("Text bytes length", textBytes.length);
console.log("Text bytes", textBytes.slice(0, 5));
console.log("Text length", text.length);
console.log("Text", text.slice(0, 100));
console.log("Img url", imgUrl);
console.log("Text url", textUrl);