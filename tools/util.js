// Copyright 2018-2025 the Deno authors. MIT license.
export const ROOT_PATH = Deno.realPathSync(new URL("..", import.meta.url));

async function getFilesFromGit(baseDir, args) {
  const { success, stdout } = await new Deno.Command("git", {
    stderr: "inherit",
    args,
  }).output();
  const output = new TextDecoder().decode(stdout);
  if (!success) {
    throw new Error("gitLsFiles failed");
  }

  const files = output
    .split("\0")
    .filter((line) => line.length > 0)
    .map(
      (filePath) => {
        try {
          return Deno.realPathSync(baseDir + "/" + filePath);
        } catch {
          return null;
        }
      },
    )
    .filter((x) => !!x);

  return files;
}

function gitLsFiles(baseDir, patterns) {
  baseDir = Deno.realPathSync(baseDir);
  const cmd = [
    "-C",
    baseDir,
    "ls-files",
    "-z",
    "--exclude-standard",
    "--cached",
    "--modified",
    "--others",
    "--",
    ...patterns,
  ];
  return getFilesFromGit(baseDir, cmd);
}

/** List all files staged for commit */
function gitStaged(baseDir, patterns) {
  baseDir = Deno.realPathSync(baseDir);
  const cmd = [
    "-C",
    baseDir,
    "diff",
    "--staged",
    "--diff-filter=ACMR",
    "--name-only",
    "-z",
    "--",
    ...patterns,
  ];
  return getFilesFromGit(baseDir, cmd);
}

/**
 *  Recursively list all files in (a subdirectory of) a git worktree.
 *    * Optionally, glob patterns may be specified to e.g. only list files with a
 *      certain extension.
 *    * Untracked files are included, unless they're listed in .gitignore.
 *    * Directory names themselves are not listed (but the files inside are).
 *    * Submodules and their contents are ignored entirely.
 *    * This function fails if the query matches no files.
 *
 * If --staged argument was provided when program is run
 * only staged sources will be returned.
 */
export async function getSources(baseDir, patterns) {
  const stagedOnly = Deno.args.includes("--staged");

  if (stagedOnly) {
    return await gitStaged(baseDir, patterns);
  } else {
    return await gitLsFiles(baseDir, patterns);
  }
}
