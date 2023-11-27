// deno-lint-ignore-file

import text from "./log.txt" with { type: "text" };

Deno.core.print(`text ${text}\n\n`);
