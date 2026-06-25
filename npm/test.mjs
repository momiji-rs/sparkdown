// Gate: the init/initSync surface, then run every CommonMark 0.31.2 spec
// example through the wasm module and require 652/652. Used by CI and release.
import { toHtml, toHtmlSync, init, initSync, ready } from "./sparkdown.mjs";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const fail = (m) => {
  console.error("FAIL:", m);
  process.exit(1);
};

// 1. Lazy: importing did not instantiate, so toHtmlSync before init must throw.
let threw = "";
try {
  toHtmlSync("# hi");
} catch (e) {
  threw = e.message;
}
if (!/initSync/.test(threw)) fail(`toHtmlSync before init should throw mentioning initSync (got: ${threw})`);

// 2. initSync() → synchronous render, no await anywhere.
const ex = initSync();
if (toHtmlSync("# hi") !== "<h1>hi</h1>\n") fail("initSync()+toHtmlSync");
// 3. Idempotent: same exports object, so only one instance.
if (initSync() !== ex) fail("initSync() not idempotent");
// 4. Mixed order: the async path reuses the same instance (no double-instantiate).
await ready;
if (initSync() !== ex) fail("await ready double-instantiated");
if ((await init()) !== ex) fail("init() returned a different instance");
if (typeof toHtml !== "function") fail("toHtml export missing");

const dir = dirname(fileURLToPath(import.meta.url));
const spec = JSON.parse(readFileSync(join(dir, "..", "tests", "fixtures", "spec.json"), "utf8"));
let pass = 0;
const failed = [];
for (const e of spec) {
  if (toHtmlSync(e.markdown) === e.html) pass++;
  else failed.push(e.example);
}

console.log(`init/initSync ok; CommonMark 0.31.2 via wasm: ${pass}/${spec.length}`);
if (pass !== spec.length) {
  console.error("FAILED examples:", failed.slice(0, 15));
  process.exit(1);
}
