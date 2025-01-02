#!/usr/bin/env -S deno run --allow-read=. --allow-write=. --allow-run=git
// Copyright 2018-2025 the Deno authors. MIT license.

import { getSources, ROOT_PATH } from "./util.js";

const copyrightYear = 2025;

const SOURCES = [
  // js and ts
  "*.js",
  "*.ts",

  // rust
  "*.rs",

  // toml
  "*Cargo.toml",
];

const COPYRIGHT_REGEX =
  /^(#|\/\/) Copyright \d+-\d+ the Deno authors. MIT license./;
const COPYRIGHT_LINE =
  `Copyright 2018-${copyrightYear} the Deno authors. MIT license.`;

// Acceptable content before the copyright line
const ACCEPTABLE_LINES =
  /^(\/\/ deno-lint-.*|\/\/ Copyright.*|\/\/ Ported.*|\s*|#!\/.*)$/;

const MISSING_ERROR_MSG = "Copyright header is missing";
const BAD_LINE_ERROR_MSG =
  "Unacceptable line appeared before copyright message";
const DATE_ERROR_MSG = "Copyright header is out-of-date";

const buffer = new Uint8Array(1024);
const textDecoder = new TextDecoder();

async function readFirstPartOfFile(filePath) {
  const file = await Deno.open(filePath, { read: true });
  try {
    const byteCount = await file.read(buffer);
    return textDecoder.decode(buffer.slice(0, byteCount ?? 0));
  } finally {
    file.close();
  }
}

async function fixFile(commentPrefix, file) {
  let fileText = await Deno.readTextFile(file);
  if (fileText.includes("\r\n")) {
    throw new Error("Cowardly refusing to fix CRLF line endings in " + file);
  }
  const newContents = [];
  let found = false;
  let nonBlank = false;
  for (const line of fileText.split("\n")) {
    if (line.length == 0) {
      if (!nonBlank) {
        console.error("Trimming empty space before copyright");
        continue;
      }
    } else {
      nonBlank = true;
    }
    if (COPYRIGHT_REGEX.test(line)) {
      if (found) {
        console.error(`Removing duplicate copyright line in ${file}`);
      } else {
        if (line != `${commentPrefix} ${COPYRIGHT_LINE}`) {
          console.error(`Freshening copyright in ${file}`);
        }
        found = true;
        newContents.push(`${commentPrefix} ${COPYRIGHT_LINE}`);
      }
      continue;
    }
    if (ACCEPTABLE_LINES.test(line)) {
      newContents.push(line);
      continue;
    }
    if (!found) {
      console.error(`Reordered copyright header in ${file}`);
      newContents.push(`${commentPrefix} ${COPYRIGHT_LINE}`);
      found = true;
    }
    newContents.push(line);
  }

  fileText = newContents.join("\n");

  await Deno.writeTextFile(file, fileText);
  return await checkFile(commentPrefix, file);
}

async function checkFile(commentPrefix, file) {
  const fileLines = (await readFirstPartOfFile(file)).split("\n");
  for (const line of fileLines) {
    // Exact match :+1:
    if (line == `${commentPrefix} ${COPYRIGHT_LINE}`) {
      return undefined;
    }

    // Wrong date, but matches regexp
    if (COPYRIGHT_REGEX.test(line)) {
      return DATE_ERROR_MSG + " (" + line + ")";
    }

    // Not a match, but acceptable
    if (ACCEPTABLE_LINES.test(line)) {
      continue;
    }

    if (COPYRIGHT_REGEX.test(await readFirstPartOfFile(file))) {
      return BAD_LINE_ERROR_MSG + " (" + line + ")";
    } else {
      return DATE_ERROR_MSG + " (" + line + ")";
    }
  }

  return MISSING_ERROR_MSG + " (" + fileLines[0] + ")";
}

export async function checkCopyright(fix = false) {
  const sourceFiles = await getSources(ROOT_PATH, SOURCES);

  const errors = [];
  const sourceFilesSet = new Set(sourceFiles);

  for (const file of sourceFilesSet) {
    if (file.endsWith("Cargo.toml")) {
      const error = await checkFile("#", file);
      if (error) {
        if (fix) {
          await fixFile("#", file);
        } else {
          errors.push(error + ": " + file);
        }
      }
      continue;
    }
    const error = await checkFile("//", file);
    if (error) {
      if (fix) {
        await fixFile("//", file);
      } else {
        errors.push(error + ": " + file);
      }
    }
  }

  // check the main license file
  const licenseText = Deno.readTextFileSync(ROOT_PATH + "/LICENSE.md");
  if (
    !licenseText.includes(`Copyright 2018-${copyrightYear} the Deno authors`)
  ) {
    errors.push(`LICENSE.md has old copyright year`);
  }

  if (errors.length > 0) {
    // show all the errors at the same time to prevent overlap with
    // other running scripts that may be outputting
    console.error(errors.join("\n"));
    throw new Error(
      `Copyright checker had ${errors.length} errors. Use --fix to repair.`,
    );
  }
}

if (import.meta.main) {
  await checkCopyright(Deno.args[0] == "--fix");
}
