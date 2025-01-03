// Copyright 2018-2025 the Deno authors. MIT license.

import { $, Crate, Repo } from "./deps.ts";

export { Crate };

export class DenoWorkspace {
  #repo: Repo;

  static get rootDirPath() {
    return $.path(import.meta).join("../../").resolve().toString();
  }

  static async load(): Promise<DenoWorkspace> {
    return new DenoWorkspace(
      await Repo.load({
        name: "deno_core",
        path: DenoWorkspace.rootDirPath,
      }),
    );
  }

  private constructor(repo: Repo) {
    this.#repo = repo;
  }

  get repo() {
    return this.#repo;
  }

  static get manifest() {
    return $.path.join(this.rootDirPath, "Cargo.toml");
  }

  get crates() {
    return this.#repo.crates;
  }

  /** Gets the deno_core dependency crates that should be published. */
  getDenoCoreDependencyCrates() {
    return this.getDenoCoreCrate()
      .immediateDependenciesInRepo().map((c) => c.crate);
  }

  getDenoCoreCrate() {
    return this.getCrate("deno_core");
  }

  getCrate(name: string) {
    return this.#repo.getCrate(name);
  }
}
