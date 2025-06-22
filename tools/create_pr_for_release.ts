#!/usr/bin/env -S deno run -A --lock=tools/deno.lock.json
// Copyright 2018-2025 the Deno authors. MIT license.
import { DenoWorkspace } from "./deno_core_workspace.ts";
import { $, createOctoKit, getGitHubRepository } from "./deps.ts";

const octoKit = createOctoKit();
const workspace = await DenoWorkspace.load();
const repo = workspace.repo;
const denoCoreCrate = workspace.getDenoCoreCrate();

// increment the core version
await denoCoreCrate.increment("minor");

// increment the dependency crate versions
for (const crate of workspace.getDenoCoreDependencyCrates()) {
  await crate.increment("minor");
}

// update the lock file
await workspace.getDenoCoreCrate().cargoUpdate("--workspace");

const originalBranch = await repo.gitCurrentBranch();
const newBranchName = `release_${denoCoreCrate.version.replace(/\./, "_")}`;

// Create and push branch
$.logStep(`Creating branch ${newBranchName}...`);
await repo.gitBranch(newBranchName);
await repo.gitAdd();
await repo.gitCommit(denoCoreCrate.version);
$.logStep("Pushing branch...");
await repo.gitPush("-u", "origin", "HEAD");

// Open PR
$.logStep("Opening PR...");
const openedPr = await octoKit.request("POST /repos/{owner}/{repo}/pulls", {
  ...getGitHubRepository(),
  base: originalBranch,
  head: newBranchName,
  draft: false,
  title: denoCoreCrate.version,
  body: getPrBody(),
});
$.log(`Opened PR at ${openedPr.data.url}`);

function getPrBody() {
  let text = `Bumped versions for ${denoCoreCrate.version}\n\n` +
    `Please ensure:\n` +
    `- [ ] Crate versions are bumped correctly\n` +
    `To make edits to this PR:\n` +
    "```shell\n" +
    `git fetch upstream ${newBranchName} && git checkout -b ${newBranchName} upstream/${newBranchName}\n` +
    "```\n";

  const actor = Deno.env.get("GH_WORKFLOW_ACTOR");
  if (actor != null) {
    text += `\ncc @${actor}`;
  }

  return text;
}
