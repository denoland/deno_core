// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
// @ts-nocheck: For error validation

// This test refers to the issue.
// https://github.com/denoland/deno_core/issues/744

Object.prototype.__defineSetter__(0, function () {});

// Need to dare to make errors.
// For example, `ReferenceError: variable is not defined`.
variable;
