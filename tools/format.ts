#!/usr/bin/env -S deno run --quiet --allow-read --allow-write --allow-run --allow-env
// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
import { main } from "./check.ts";

await main("format", Deno.args[0]);
