#!/usr/bin/env -S deno run --quiet -RWE --allow-run
// Copyright 2018-2025 the Deno authors. MIT license.
import { $ } from "@deno/rust-automation";

const CLIPPY_FEATURES =
  `deno_core/default deno_core/include_js_files_for_snapshotting deno_core/unsafe_runtime_options deno_core/unsafe_use_unprotected_platform`;

async function main() {
  if (Deno.args[0] == "--fix") {
    $.logGroup("Linting (--fix)");
    $.log("copyright");
    await $`tools/copyright_checker.js --fix`;
    $.log("cargo clippy");
    await $`cargo clippy --fix --allow-dirty --allow-staged --locked --release --features ${CLIPPY_FEATURES} --all-targets -- -D clippy::all`;
    $.logGroupEnd();
    return;
  }

  $.logGroup("Linting");
  $.log("deno check tools/");
  await $`deno check --config=tools/deno.json tools/`;
  $.log("copyright");
  await $`tools/copyright_checker.js`;
  $.log("deno lint");
  await $`deno lint`;
  $.log("tsc");
  await $`deno run --allow-read --allow-env npm:typescript@5.5.3/tsc --noEmit -p testing/tsconfig.json`;
  $.log("cargo clippy");
  await $`cargo clippy --locked --release --features ${CLIPPY_FEATURES} --all-targets -- -D clippy::all`;
  $.logGroupEnd();
}

await main();
