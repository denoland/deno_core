// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
// Re-export fields from `globalThis.__bootstrap` so that embedders using
// ES modules can import these symbols instead of capturing the bootstrap ns.
const bootstrap = globalThis.__bootstrap;
const { core, internals, primordials } = bootstrap;

export { core, internals, primordials };
