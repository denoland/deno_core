// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.

import { $, Repo } from "./deps.ts";

export class DenoWorkspace {
  #repo: Repo;

  static get rootDirPath() {
    const currentDirPath = $.path.dirname($.path.fromFileUrl(import.meta.url));
    return $.path.resolve(currentDirPath, "../");
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

  get crates() {
    return this.#repo.crates;
  }

  /** Gets the deno_core dependency crates that should be published. */
  getDenoCoreDependencyCrates() {
    return this.getDenoCoreCrate()
      .descendantDependenciesInRepo();
  }

  getDenoCoreCrate() {
    return this.getCrate("deno_core");
  }

  getCrate(name: string) {
    return this.#repo.getCrate(name);
  }
}
