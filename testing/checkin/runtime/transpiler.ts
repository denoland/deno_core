// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
import { op_transpile } from "ext:core/ops";

export class Transpiler {
  constructor() {
  }

  transpile(specifier: string, source: string) {
    return op_transpile(specifier, source);
  }
}
