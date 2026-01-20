# Import Defer Implementation Notes

## TC39 Proposal
- **Proposal**: https://github.com/tc39/proposal-defer-import-eval
- **Spec**: https://tc39.es/proposal-defer-import-eval/
- **Stage**: 3 (as of February 2025)

## Syntax
```javascript
// Static import defer - namespace only
import defer * as ns from "./module.js";

// Dynamic import defer
const ns = await import.defer("./module.js");
```

## Semantics
1. Module and its dependencies are **loaded and parsed** (execution-ready)
2. Module is **NOT evaluated** until first property access on namespace
3. On first property access, module is evaluated **synchronously**
4. The namespace acts as a proxy that triggers evaluation on any [[Get]] operation

## V8 Support Status (as of Jan 2025)

### V8 Flag
- `--js-defer-import-eval` enables parser support
- Added to `core/runtime/setup.rs` in base_flags

### Current Issue
V8 14.5 (rusty_v8 145.0.0) has:
- ✅ Parser support (syntax recognized)
- ❌ Incomplete runtime support (crashes)

**Crash locations:**
- Static import: `SourceTextModule::RunInitializationCode` during `Module::Instantiate`
- Dynamic import: `Runtime_DynamicImportCall` -> `GetImportAttributesFromArgument`

### V8 Implementation Progress
- Chrome tracking: https://issues.chromium.org/issues/398218423
- Recent commits to V8 main: "[defer-import-eval] deferred import evaluation for ES modules" by Caio Lima
- Full support likely coming in future V8/rusty_v8 versions

## deno_core Implementation

### Files Modified
1. `core/runtime/setup.rs` - Added V8 flag
2. `core/modules/map.rs` - Separated Defer from Evaluation phase handling

### Key Code Location: `core/modules/map.rs` ~line 1966
```rust
match state.phase {
  ModuleImportPhase::Evaluation => {
    // Normal: instantiate + evaluate
  }
  ModuleImportPhase::Defer => {
    // Defer: instantiate only, resolve with namespace
    // V8 handles deferred namespace that triggers eval on access
  }
  ModuleImportPhase::Source => {
    // Source phase imports (for WASM)
  }
}
```

### Related Code Paths
- `core/runtime/bindings.rs:691` - `host_import_module_with_phase_dynamically_callback`
- `core/modules/recursive_load.rs:273` - Phase handling during module loading
- `core/modules/mod.rs:705` - `ModuleImportPhase` enum definition

## Test Files
- `testing/integration/import_defer/import_defer.js` - Main test
- `testing/integration/import_defer/deferred.js` - Deferred module
- `testing/integration/import_defer/import_defer.out` - Expected output
- Test is **commented out** in `testing/lib.rs` until V8 support is complete

## Expected Test Output
```
before import defer
after import defer, before access
deferred module evaluated
value: 42
after first access
add: 3
done
```

## To Complete Implementation

When V8 runtime support is ready:
1. Update rusty_v8 to version with complete defer support
2. Uncomment `import_defer` in `testing/lib.rs`
3. Run test: `cargo test -p deno_core_testing import_defer`
4. If V8 handles deferred namespace correctly, test should pass
5. If not, may need to implement JavaScript Proxy-based deferred namespace

## Alternative: Proxy-Based Implementation

If V8's native deferred namespace doesn't work as expected, could implement in JS:
```javascript
function createDeferredNamespace(module, evaluateFn) {
  let evaluated = false;
  let namespace = null;
  return new Proxy({}, {
    get(target, prop) {
      if (!evaluated) {
        evaluateFn();
        namespace = module.getNamespace();
        evaluated = true;
      }
      return namespace[prop];
    },
    // Also need: has, ownKeys, getOwnPropertyDescriptor
  });
}
```

## References
- WebKit implementation: https://github.com/WebKit/WebKit/pull/40453
- TypeScript 5.9 support: https://www.typescriptlang.org/docs/handbook/release-notes/typescript-5-9.html
- Babel plugin: https://babeljs.io/docs/babel-plugin-proposal-import-defer
