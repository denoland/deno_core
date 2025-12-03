#!/usr/bin/env -S deno run --quiet -RWE --allow-run
// Copyright 2018-2025 the Deno authors. MIT license.
import { $ } from "@deno/rust-automation";

if (Deno.args[0] == "--check") {
  await $`deno run -A npm:dprint@0.47.6 check`;
} else {
  await $`deno run -A npm:dprint@0.47.6 fmt`;
}
