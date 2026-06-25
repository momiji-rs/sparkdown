// Conformance gate for the npm package: run every CommonMark 0.31.2 spec
// example through the wasm module and require 652/652. Used by CI and release.
import { toHtmlSync, ready } from "./sparkdown.mjs";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const dir = dirname(fileURLToPath(import.meta.url));
const spec = JSON.parse(
  readFileSync(join(dir, "..", "tests", "fixtures", "spec.json"), "utf8"),
);

await ready;
let pass = 0;
const failed = [];
for (const ex of spec) {
  if (toHtmlSync(ex.markdown) === ex.html) pass++;
  else failed.push(ex.example);
}

// Sanity-check the API surface too.
if (typeof (await import("./sparkdown.mjs")).toHtml !== "function") {
  console.error("toHtml export missing");
  process.exit(1);
}

console.log(`CommonMark 0.31.2 via wasm: ${pass}/${spec.length}`);
if (pass !== spec.length) {
  console.error("FAILED examples:", failed.slice(0, 15));
  process.exit(1);
}
