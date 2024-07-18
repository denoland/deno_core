// deno-lint-ignore-file no-explicit-any
// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

export function op_log_debug(...any: any[]): any;
export function op_log_info(...any: any[]): any;

export function op_test_register(...any: any[]): any;

export function op_async_throw_error_deferred(...any: any[]): any;
export function op_async_throw_error_eager(...any: any[]): any;
export function op_async_throw_error_lazy(...any: any[]): any;
export function op_error_context_async(...any: any[]): any;
export function op_error_context_sync(...any: any[]): any;
export function op_error_custom_sync(...any: any[]): any;
export function op_fs_read_text_file(path: string): string;
export function op_fs_write_text_file(path: string, content: string): void;
export function op_transpile(
  specifier: string,
  source: string,
): [string, string | undefined];

export function op_worker_await_close(...any: any[]): any;
export function op_worker_parent(...any: any[]): any;
export function op_worker_recv(...any: any[]): any;
export function op_worker_send(...any: any[]): any;
export function op_worker_spawn(...any: any[]): any;
export function op_worker_terminate(...any: any[]): any;
