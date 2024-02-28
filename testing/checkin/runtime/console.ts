// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

import { op_log_debug, op_log_info } from "ext:core/ops";
import { core } from "ext:core/mod.js";

export const console = {
  debug(...args: string[]) {
    op_log_debug(core.consoleStringify(...args));
  },

  log(...args: string[]) {
    op_log_info(core.consoleStringify(...args));
  },
};
