// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.

import { op_log_debug, op_log_info } from "ext:core/ops";

export const console = {
  debug(...args: string[]) {
    op_log_debug(args.join(","));
  },

  log(...args: string[]) {
    op_log_info(args.join(","));
  },
};
