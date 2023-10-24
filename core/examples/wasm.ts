// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.

export declare function op_wasm(handle: u32): void;

export function call(handle: u32): void {
  op_wasm(handle);
}
