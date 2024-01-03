export async function wasmShim(wasmBytes) {
  const wasmMod = await WebAssembly.compile(wasmBytes);
  const requestedImports = WebAssembly.Module.imports(wasmMod);
  const importedModules = await Promise.all(
    requestedImports.map((i) => i.module).map((m) => import(m)),
  );
  const importsObject = {};
  for (let i = 0; i < requestedImports.length; i++) {
    const importedModule = importedModules[i];
    const requestedImport = requestedImports[i];
    if (typeof importsObject[requestedImport.module] === "undefined") {
      importsObject[requestedImport.module] = {};
    }
    const import_ = importedModule[requestedImport.name];
    importsObject[requestedImport.module][requestedImport.name] = import_;
  }
  const result = await WebAssembly.instantiate(wasmMod, importsObject);
  return result;
}
