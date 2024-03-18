import { isContext, runInNewContext } from "./vm.ts";
import * as vm from "./vm.ts";

const TEST_FNS = [];

/**
 * Assert a value is truthy.
 */
export function assert(value, message) {
  if (!value) {
    const assertion = "Failed assertion" + (message ? `: ${message}` : "");
    console.debug(assertion);
    throw new Error(assertion);
  }
}

/**
 * Fails a test.
 */
export function fail(reason) {
  console.debug("Failed: " + reason);
  throw new Error("Failed: " + reason);
}

/**
 * Assert two values match (==).
 */
export function assertEquals(a1, a2) {
  assert(a1 == a2, `${a1} != ${a2}`);
}

function assertThrows(fn) {
  let didThrow = false;
  try {
    fn();
  } catch (e) {
    didThrow = true;
  }
  assert(didThrow, "Function did not throw");
}

function test({ name, fn }) {
  TEST_FNS.push({ name, fn });
}

test({
  name: "vm runInNewContext",
  fn() {
    const two = runInNewContext("1 + 1");
    assertEquals(two, 2);
  },
});

test({
  name: "vm runInNewContext sandbox",
  fn() {
    assertThrows(() => runInNewContext("Deno"));
    // deno-lint-ignore no-var
    var a = 1;
    assertThrows(() => runInNewContext("a + 1"));

    runInNewContext("a = 2");
    assertEquals(a, 1);
  },
});

// https://github.com/webpack/webpack/blob/87660921808566ef3b8796f8df61bd79fc026108/lib/javascript/JavascriptParser.js#L4329
test({
  name: "vm runInNewContext webpack magic comments",
  fn() {
    const webpackCommentRegExp = new RegExp(
      /(^|\W)webpack[A-Z]{1,}[A-Za-z]{1,}:/,
    );
    const comments = [
      'webpackChunkName: "test"',
      'webpackMode: "lazy"',
      "webpackPrefetch: true",
      "webpackPreload: true",
      "webpackProvidedExports: true",
      'webpackChunkLoading: "require"',
      'webpackExports: ["default", "named"]',
    ];

    for (const comment of comments) {
      const result = webpackCommentRegExp.test(comment);
      assertEquals(result, true);

      const [[key, _value]] = Object.entries(
        runInNewContext(`(function(){return {${comment}};})()`),
      );
      const expectedKey = comment.split(":")[0].trim();
      assertEquals(key, expectedKey);
    }
  },
});

test({
  name: "vm isContext",
  fn() {
    // Currently we do not expose VM contexts so this is always false.
    const obj = {};
    assertEquals(isContext(obj), false);
    assertEquals(isContext(globalThis), false);
    const sandbox = runInNewContext("{}");
    // assertEquals(isContext(sandbox), false);
  },
});

test({
  name: "vm new Script()",
  fn() {
    const script = new vm.Script(`
function add(a, b) {
  return a + b;
}

const x = add(1, 2);
x
`);

    const value = script.runInThisContext2();
    console.log("script run in this context", value);
    assertEquals(value, 3);
  },
});

test({
  name: "vm createContext",
  fn() {
    globalThis.globalVar = 3;

    const context = { globalVar: 1 };
    vm.createContext(context);
    vm.runInContext("globalVar *= 2", context);
    assertEquals(context.globalVar, 2);
    assertEquals(globalThis.globalVar, 2);
  },
});

function runTests() {
  for (const { name, fn } of TEST_FNS) {
    console.log("Running ", name);
    fn();
  }
}

runTests();
