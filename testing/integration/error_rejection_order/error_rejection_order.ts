// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.
for (let i = 0; i < 100; i++) {
  Promise.reject(i);
}
