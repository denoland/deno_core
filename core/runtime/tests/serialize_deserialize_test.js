// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
"use strict";

function assert(cond) {
  if (!cond) {
    throw Error("assert");
  }
}

function assertArrayEquals(a1, a2) {
  if (a1.length !== a2.length) throw Error("assert");

  for (const index in a1) {
    if (a1[index] !== a2[index]) {
      throw Error(`assert: (index ${index}) ${a1[index]} !== ${a2[index]}`);
    }
  }
}

function testIssue20727() {
  // https://github.com/denoland/deno/issues/20727
  const ab = new ArrayBuffer(10);
  Deno.core.ops.op_serialize(
    { ab },
    { transferredArrayBuffers: [ab] },
  );

  const data = {
    array1: new Uint32Array([]),
    array2: new Float32Array([]),
  };

  const transferredArrayBuffers = [
    data.array1.buffer,
    data.array2.buffer,
  ];
  const serializedMultipleTransferredBuffers = Deno.core.ops.op_serialize(
    { id: 2, data },
    { transferredArrayBuffers },
  );
  // should not throw
  Deno.core.ops.op_deserialize(
    serializedMultipleTransferredBuffers,
    { transferredArrayBuffers },
  );
}


function main() {
  const emptyString = "";
  const emptyStringSerialized = [255, 15, 34, 0];
  assertArrayEquals(
    Deno.core.ops.op_serialize(emptyString),
    emptyStringSerialized,
  );
  assert(
    Deno.core.ops.op_deserialize(
      new Uint8Array(emptyStringSerialized),
    ) ===
      emptyString,
  );

  const primitiveValueArray = ["test", "a", null, undefined];
  // deno-fmt-ignore
  const primitiveValueArraySerialized = [
    255, 15, 65, 4, 34, 4, 116, 101, 115, 116,
    34, 1, 97, 48, 95, 36, 0, 4,
  ];
  assertArrayEquals(
    Deno.core.ops.op_serialize(primitiveValueArray),
    primitiveValueArraySerialized,
  );

  assertArrayEquals(
    Deno.core.ops.op_deserialize(
      new Uint8Array(primitiveValueArraySerialized),
    ),
    primitiveValueArray,
  );

  const circularObject = { test: null, test2: "dd", test3: "aa" };
  circularObject.test = circularObject;
  // deno-fmt-ignore
  const circularObjectSerialized = [
    255, 15, 111, 34, 4, 116, 101, 115,
    116, 94, 0, 34, 5, 116, 101, 115,
    116, 50, 34, 2, 100, 100, 34, 5,
    116, 101, 115, 116, 51, 34, 2, 97,
    97, 123, 3,
  ];

  assertArrayEquals(
    Deno.core.ops.op_serialize(circularObject),
    circularObjectSerialized,
  );

  const deserializedCircularObject = Deno.core.ops.op_deserialize(
    new Uint8Array(circularObjectSerialized),
  );
  assert(deserializedCircularObject.test == deserializedCircularObject);

  testIssue20727();
}

main();
