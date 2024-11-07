// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
import { op_fs_read_text_file, op_fs_write_text_file } from "ext:core/ops";

export function readTextFile(filePath: string): string {
  return op_fs_read_text_file(filePath);
}

export function writeTextFile(filePath: string, content: string) {
  op_fs_write_text_file(filePath, content);
}
