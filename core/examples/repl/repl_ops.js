const context = Deno.core.ops.repl_start();

while (true) {
  const input = Deno.core.ops.repl_read();
  const output = Deno.core.ops.repl_evaluate(context, input);
  console.log(output);
}

repl.context.Foobar = {...};