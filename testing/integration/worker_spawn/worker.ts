// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
import { Worker } from "checkin:worker";
Worker.parent.sendMessage("hello from client");
const message = await Worker.parent.receiveMessage();
console.log(`worker got from main "${message}"`);
