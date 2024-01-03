import { sleep } from "./import_inner_inner.mjs";
export { add } from "./import_inner_inner.mjs";

console.log("import_inner.js before");

await sleep(100);

console.log("import_inner.js after");
