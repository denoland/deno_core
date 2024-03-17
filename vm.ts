const core = Deno.core;
const {
  op_vm_run_in_new_context,
  op_vm_make_context,
  op_vm_is_context,
  op_script_run_in_context,
} = core.ops;

function notImplemented(name) {
  throw new Error(`The API ${name} is not yet implemented`);
}

const kParsingContext = Symbol("script parsing context");
const kVmBreakFirstLineSymbol = Symbol("kVmBreakFirstLineSymbol");

export class Script {
  code;
  constructor(code, _options = {}) {
    this.code = `${code}`;
  }

  runInThisContext(_options) {
    const [result, error] = core.evalContext(this.code, "data:");
    if (error) {
      throw error.thrown;
    }
    return result;
  }

  runInContext(contextifiedObject, options) {
    validateContext(contextifiedObject);
    const { breakOnSigint, args } = getRunInContextArgs(
      contextifiedObject,
      options,
    );
    // TODO:
    // if (breakOnSigint && process.listenerCount("SIGINT") > 0) {
    //   return sigintHandlersWrap(super.runInContext, this, args);
    // }
    return op_script_run_in_context(
      this,
      args[0],
      args[1],
      args[2],
      args[3],
      args[4],
    );
    // return ReflectApply(super.runInContext, this, args);
    notImplemented("Script.prototype.runInContext");
  }

  runInNewContext(contextObject, options) {
    if (options) {
      console.warn(
        "Script.runInNewContext options are currently not supported",
      );
    }
    return op_vm_run_in_new_context(this.code, contextObject);
  }

  createCachedData() {
    notImplemented("Script.prototyp.createCachedData");
  }
}

const kEmptyObject = Object.freeze({ __proto__: null });
let defaultContextNameIndex = 1;
export function createContext(contextObject = {}, options = kEmptyObject) {
  if (isContext(contextObject)) {
    return contextObject;
  }

  // TODO: validateObject(options, "options");

  const {
    name = `VM Context ${defaultContextNameIndex++}`,
    origin,
    codeGeneration,
    microtaskMode,
    importModuleDynamically,
  } = options;

  // validateString(name, "options.name");
  // if (origin !== undefined) {
  //   validateString(origin, "options.origin");
  // }
  // if (codeGeneration !== undefined) {
  //   validateObject(codeGeneration, "options.codeGeneration");
  // }

  let strings = true;
  let wasm = true;
  if (codeGeneration !== undefined) {
    ({ strings = true, wasm = true } = codeGeneration);
    // validateBoolean(strings, "options.codeGeneration.strings");
    // validateBoolean(wasm, "options.codeGeneration.wasm");
  }

  // validateOneOf(microtaskMode, "options.microtaskMode", [
  //   "afterEvaluate",
  //   undefined,
  // ]);
  const microtaskQueue = microtaskMode === "afterEvaluate";

  // const hostDefinedOptionId = getHostDefinedOptionId(
  //   importModuleDynamically,
  //   name,
  // );

  op_vm_make_context(
    contextObject,
    name,
    origin,
    strings,
    wasm,
    microtaskQueue,
    // hostDefinedOptionId,
  );
  // Register the context scope callback after the context was initialized.
  // if (importModuleDynamically !== undefined) {
  //   registerImportModuleDynamically(contextObject, importModuleDynamically);
  // }
  return contextObject;
}

export function createScript(code, options) {
  return new Script(code, options);
}

function validateContext(contextifiedObject) {
  if (!(isContext(contextifiedObject))) {
    throw new TypeError("The provided context is not a contextified object");
  }
}

function getRunInContextArgs(contextifiedObject, options = kEmptyObject) {
  // validateObject(options, "options");

  let timeout = options.timeout;
  if (timeout === undefined) {
    timeout = -1;
  } else {
    // TODO:
    // validateUint32(timeout, "options.timeout", true);
  }

  const {
    displayErrors = true,
    breakOnSigint = false,
    [kVmBreakFirstLineSymbol]: breakFirstLine = false,
  } = options;

  // TODO:
  // validateBoolean(displayErrors, "options.displayErrors");
  // validateBoolean(breakOnSigint, "options.breakOnSigint");

  return {
    breakOnSigint,
    args: [
      contextifiedObject,
      timeout,
      displayErrors,
      breakOnSigint,
      breakFirstLine,
    ],
  };
}

export function runInContext(
  code,
  contextifiedObject,
  options,
) {
  validateContext(contextifiedObject);
  if (typeof options === "string") {
    options = {
      filename: options,
      [kParsingContext]: contextifiedObject,
    };
  } else {
    options = {
      ...options,
      [kParsingContext]: contextifiedObject,
    };
  }
  return createScript(code, options).runInContext(contextifiedObject, options);
}

export function runInNewContext(
  code,
  contextObject,
  options,
) {
  if (options) {
    console.warn("vm.runInNewContext options are currently not supported");
  }
  return op_vm_run_in_new_context(code, contextObject);
}

export function runInThisContext(
  code,
  options,
) {
  return createScript(code, options).runInThisContext(options);
}

export function isContext(context) {
  return op_vm_is_context(context);
}

export function compileFunction(_code, _params, _options) {
  notImplemented("compileFunction");
}

export function measureMemory(_options) {
  notImplemented("measureMemory");
}

export default {
  Script,
  createContext,
  createScript,
  runInContext,
  runInNewContext,
  runInThisContext,
  isContext,
  compileFunction,
  measureMemory,
};
