import text from "./log.txt" with { type: "text" };
import styles from "./styles.css" with { type: "css-module" };

// Deno.core.print(`text ${text}\n\n`);

Deno.core.print(`styles .foo ${styles.foo}\n`);
Deno.core.print(`styles .bar ${styles.bar}\n`);