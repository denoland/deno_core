const transpiler = new Transpiler();

const SOURCE = `export function a(): string {
  return "a";
}

function spawn() {
  const b = new Deno.Command();

  b.spawn();
}

interface Foobar {
  a: string;
  b: number;
  c: boolean;
}

function runSpawn() {
  spawn();
}

runSpawn();`;

const SPECIFIER = "file:///a.ts";
const TRANSPILED_SOURCE_FILE_PATH = "./a.js";
const SOURCE_MAP_FILE_PATH = "./a.js.map";
const [transpiledSource, sourceMap] = transpiler.transpile(SPECIFIER, SOURCE);
writeTextFile(TRANSPILED_SOURCE_FILE_PATH, transpiledSource);
writeTextFile(SOURCE_MAP_FILE_PATH, sourceMap);
