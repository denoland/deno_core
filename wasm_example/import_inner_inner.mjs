console.log("import_inner_inner.js before");

export async function sleep(timeout) {
  return new Promise((resolve) => {
    Deno.core.queueTimer(
      Deno.core.getTimerDepth() + 1,
      false,
      timeout,
      resolve,
    );
  });
}
await sleep(100);

console.log("import_inner_inner.js after");

const abc = 1 + 2;
export function add(a, b) {
  console.log(`abc: ${abc}`);
  return a + b;
}
