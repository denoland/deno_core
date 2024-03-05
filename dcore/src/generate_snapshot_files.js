const PATH = import.meta.dirname + "/snapshot/main.js";
const MAIN_SOURCE = [];

function generateSnapshotFile() {
  const contents = [];

  for (let i = 0; i < 100; i++) {
    contents.push(`export function fn${i}(a, b) {`);

    if (i % 10 === 0) {
      contents.push(`  return a ^ b`);
    } else if (i % 5 === 0) {
      contents.push(`  return a / b`);
    } else if (i % 3 === 0) {
      contents.push(`  return a * b`);
    } else if (i % 2 === 0) {
      contents.push(`  return a - b`);
    } else {
      contents.push(`  return a + b`);
    }
    contents.push(`}`);
  }

  return contents.join("\n");
}

for (let i = 1; i <= 50; i++) {
  MAIN_SOURCE.push(`import * as mod${i} from "./${i}.js";`);

  const filename = import.meta.dirname + `/snapshot/${i}.js`;
  const contents = generateSnapshotFile();
  Deno.writeTextFileSync(
    filename,
    contents,
  );
}

Deno.writeTextFileSync(
  PATH,
  MAIN_SOURCE.join("\n"),
);
