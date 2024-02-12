let config;

try {
  config = await import("./module.js");
} catch (err) {
  console.log("caught error", err);
  console.log("rethrowing");
  throw err;
}
