// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
import { Worker } from "checkin:worker";
console.log("main started");
const worker = new Worker(import.meta.url, "./worker.ts");
console.log("main worker booted");
const message = await worker.receiveMessage();
console.log(`main got from worker "${message}"`);
worker.terminate();
await worker.closed;
console.log("main exiting");
