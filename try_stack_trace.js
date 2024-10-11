function hello() {
  Deno.core.ops.op_print_stack_trace();

  Deno.core.ops.op_print_stack_trace_async();
}

function bar() {
  hello();
}

bar();
