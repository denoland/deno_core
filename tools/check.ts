#!/usr/bin/env -S deno run --quiet --allow-read --allow-write --allow-run --allow-env
// Copyright 2018-2025 the Deno authors. MIT license.

import $, * as dax from "https://deno.land/x/dax@0.39.2/mod.ts";

const isCI = !!Deno.env.get("CI");
const divider =
  "--------------------------------------------------------------";

class CommandState {
  stdout: string = "";
  stderr: string = "";
  lastLine: string = "";
  lastLineTimestamp: number = 0;
  success: boolean;
  running: boolean;

  constructor(public name: string) {
    this.success = true;
    this.running = true;
  }

  makeReader(which: "stdout" | "stderr", callback: () => void) {
    // deno-lint-ignore no-this-alias
    const self = this;
    const textDecoder = new TextDecoderStream();
    textDecoder.readable.pipeTo(
      new WritableStream({
        write(chunk) {
          self[which] += chunk;
          if (chunk.includes("\n")) {
            const lines = self[which].split("\n");
            while (lines.length > 0) {
              const line = lines[lines.length - 1].trim().replace(
                // deno-lint-ignore no-control-regex
                /[\u001b\u009b][[()#;?]*(?:[0-9]{1,4}(?:;[0-9]{0,4})*)?[0-9A-ORZcf-nqry=><]/g,
                "",
              );
              if (line.length == 0) {
                lines.pop();
                continue;
              }

              if (line != self.lastLine) {
                self.lastLine = line;
                self.lastLineTimestamp = new Date().valueOf();
                callback();
              }
              break;
            }
          }
        },
      }),
    );
    return textDecoder.writable;
  }

  toString() {
    if (!this.running) {
      if (this.success) {
        return `✅ ${this.name}`;
      } else {
        return `❌ ${this.name}`;
      }
    }

    return `⏳ ${this.name}`;
  }
}

async function runCommands(
  mode: string,
  commands: { [name: string]: dax.CommandBuilder },
) {
  function makeProgress() {
    if (isCI) {
      return null;
    }
    return $.progress(mode);
  }

  const pb = makeProgress();
  const promises: Promise<void>[] = [];
  const processes: Set<CommandState> = new Set();

  function updateProgress(final: boolean = false) {
    if (!pb) {
      if (final) {
        console.log(divider);
        console.log("Done: " + [...processes.values()].flat().join(", "));
      }
      return;
    }
    const processString = [...processes.values()].flat().join(", ");
    let lastLine = "";
    let lastTime = 0;
    let complete = 0;
    for (const process of processes) {
      if (process.running && process.lastLineTimestamp > lastTime) {
        let line = process.lastLine;
        if (line.length > 50) {
          line = line.slice(0, 50) + "᠁";
        }
        lastLine = ` (${line})`;
        lastTime = process.lastLineTimestamp;
      }
      if (!process.running) {
        complete++;
      }
    }
    pb.length(processes.size);
    pb.position(complete);
    pb.message(processString + lastLine);
  }

  for (const [name, command] of Object.entries(commands)) {
    const state = new CommandState(name);
    processes.add(state);

    const promise = (async () => {
      try {
        await command
          .stdout(state.makeReader("stdout", updateProgress))
          .stderr(state.makeReader("stderr", updateProgress));
        if (isCI) {
          console.log(`✅ ${state.name} passed`);
          console.log(state.stderr.trim());
          console.log(state.stdout.trim());
        }
      } catch (e) {
        state.success = false;
        $.log(`❌ ${state.name} failed!`);
        if (!e.message.includes("Exited with code")) {
          console.log(e);
        }
        $.log(divider);
        $.log(state.stderr.trim());
        $.log(state.stdout.trim());
        $.log(divider);
        updateProgress();
      } finally {
        state.running = false;
        updateProgress();
      }
      return;
    })();
    promises.push(promise);
  }

  updateProgress();
  await Promise.all(promises);

  updateProgress(true);

  if (pb) {
    pb.noClear();
    pb.prefix("Done");
    pb.forceRender();
    pb.finish();
  }

  for (const process of processes) {
    if (!process.success) {
      Deno.exit(1);
    }
  }
}

const CLIPPY_FEATURES =
  `deno_core/default deno_core/include_js_files_for_snapshotting deno_core/unsafe_runtime_options deno_core/unsafe_use_unprotected_platform`;

export async function main(command: string, flag: string) {
  if (command == "format") {
    const check = flag == "--check";
    if (check) {
      await runCommands("Formatting (--check)", {
        "dprint fmt": $`deno run -A npm:dprint@0.47.6 check`,
      });
    } else {
      await runCommands("Formatting", {
        "dprint fmt": $`deno run -A npm:dprint@0.47.6 fmt`,
      });
    }
  } else if (command == "lint") {
    const fix = flag == "--fix";
    if (fix) {
      await runCommands("Linting (--fix)", {
        "copyright": $`tools/copyright_checker.js --fix`,
        "cargo clippy":
          $`cargo clippy --fix --allow-dirty --allow-staged --locked --release --features ${CLIPPY_FEATURES} --all-targets -- -D clippy::all`,
      });
    } else {
      await runCommands("Linting", {
        "deno check tools/": $`deno check ${
          [...Deno.readDirSync("tools")].map((f) => `tools/${f.name}`)
        }`,
        "copyright": $`tools/copyright_checker.js`,
        "deno lint": $`deno lint`,
        "tsc":
          $`deno run --allow-read --allow-env npm:typescript@5.5.3/tsc --noEmit -p testing/tsconfig.json`,
        "cargo clippy":
          $`cargo clippy --locked --release --features ${CLIPPY_FEATURES} --all-targets -- -D clippy::all`,
      });
    }
  }
}

if (import.meta.main) {
  if (Deno.args.length == 0) {
    console.error("Usage:");
    console.error("  check.ts lint [--fix]");
    console.error("  check.ts format [--check]");
    Deno.exit(1);
  }
  const command = Deno.args[0];
  const flag = Deno.args[1];
  await main(command, flag);
}
