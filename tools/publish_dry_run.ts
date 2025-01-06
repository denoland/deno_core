#!/usr/bin/env -S deno run -A --lock=tools/deno.lock.json
// Copyright 2018-2025 the Deno authors. MIT license.
import { Crate, DenoWorkspace } from "./deno_core_workspace.ts";

const workspace = await DenoWorkspace.load();
const repo = workspace.repo;
const denoCoreCrate = workspace.getDenoCoreCrate();

const allCrates: { [key: string]: Crate } = {};
allCrates[denoCoreCrate.name] = denoCoreCrate;

for (const crate of workspace.getDenoCoreDependencyCrates()) {
  allCrates[crate.name] = crate;
}

// We have the crates in the correct publish order here
const cratesInOrder = repo.getCratesPublishOrder().map((c) => allCrates[c.name])
  .filter((c) => !!c);

for (const crate of cratesInOrder) {
  await crate.increment("minor");
}

const originalManifest = Deno.readTextFileSync(DenoWorkspace.manifest);
let manifest = originalManifest;
for (const crate of cratesInOrder) {
  const re = new RegExp(`^(\\b${crate.name}\\b\\s=.*)}$`, "gm");
  manifest = manifest.replace(re, '$1, registry = "upstream" }');
}

Deno.writeTextFileSync(DenoWorkspace.manifest, manifest);

for (const crate of cratesInOrder) {
  await crate.publish(
    "-p",
    crate.name,
    "--allow-dirty",
    "--registry",
    "upstream",
    "--token",
    "Zy9HhJ02RJmg0GCrgLfaCVfU6IwDfhXD",
    "--config",
    'registries.upstream.index="sparse+http://localhost:8000/api/v1/crates/"',
  );
}
