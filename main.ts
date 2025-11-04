const { op_thingy } = Deno.core.ops;

op_thingy(new Uint16Array([1, 2, 3]));
