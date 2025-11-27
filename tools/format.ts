#!/usr/bin/env -S deno run --quiet -RWE --allow-run
// Copyright 2018-2025 the Deno authors. MIT license.
import { $ } from "@deno/rust-automation";

const flag = Deno.args[0];
const check = flag == "--check";
if (check) {
  await $`deno run -A npm:dprint@0.47.6 check`;
} else {
  $`deno run -A npm:dprint@0.47.6 fmt`;
}
