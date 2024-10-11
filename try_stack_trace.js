function hello() {
  Deno.core.ops.op_print_stack_trace();
}

function bar() {
  hello();
}

bar();
