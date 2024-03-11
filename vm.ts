const core = Deno.core;
const { op_vm_run_in_new_context } = core.ops;

function notImplemented(name) {
  throw new Error(`The API ${name} is not yet implemented`);
}

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

  runInContext(_contextifiedObject, _options) {
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

export function createContext(_contextObject, _options) {
  notImplemented("createContext");
}

export function createScript(code, options) {
  return new Script(code, options);
}

export function runInContext(
  _code,
  _contextifiedObject,
  _options,
) {
  notImplemented("runInContext");
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

export function isContext(_maybeContext) {
  // TODO(@littledivy): Currently we do not expose contexts so this is always false.
  return false;
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
