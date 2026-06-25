// Gate: (0) the init/initSync surface; (1) CommonMark 0.31.2 — all 652 examples
// with every GFM flag OFF must still match (same engine); (2) GFM spot checks.
import { toHtmlSync, init, initSync, ready } from "./sparkdown.mjs";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const fail = (m) => {
  console.error("FAIL:", m);
  process.exit(1);
};

// 0. Lazy import → toHtmlSync before init throws; initSync() is sync + idempotent.
let threw = "";
try {
  toHtmlSync("# hi");
} catch (e) {
  threw = e.message;
}
if (!/initSync/.test(threw)) fail(`toHtmlSync before init should throw mentioning initSync (got: ${threw})`);
const ex = initSync();
if (toHtmlSync("# hi") !== "<h1>hi</h1>\n") fail("initSync()+toHtmlSync");
if (initSync() !== ex) fail("initSync() not idempotent");
await ready;
if (initSync() !== ex) fail("await ready double-instantiated");
if ((await init()) !== ex) fail("init() returned a different instance");

const dir = dirname(fileURLToPath(import.meta.url));
const spec = JSON.parse(readFileSync(join(dir, "..", "tests", "fixtures", "spec.json"), "utf8"));
const PURE = { strikethrough: false, tasklist: false, autolink: false, tagfilter: false, tables: false, hardWraps: false };
let pass = 0;
const failed = [];
for (const e of spec) {
  if (toHtmlSync(e.markdown, PURE) === e.html) pass++;
  else failed.push(e.example);
}

const checks = [
  ["~~x~~", "<p><del>x</del></p>\n"],
  ["www.example.com", '<p><a href="http://www.example.com">www.example.com</a></p>\n'],
  ["- [x] done", '<ul>\n<li><input checked="" disabled="" type="checkbox"> done</li>\n</ul>\n'],
];
for (const [md, want] of checks) {
  const got = toHtmlSync(md);
  if (got !== want) {
    fail(`GFM check: ${JSON.stringify(md)}\n  want ${JSON.stringify(want)}\n  got  ${JSON.stringify(got)}`);
  }
}

console.log(`init/initSync ok; CommonMark 0.31.2 via wasm (gfm off): ${pass}/${spec.length}; GFM spot checks ok`);
if (pass !== spec.length) {
  console.error("FAILED examples:", failed.slice(0, 15));
  process.exit(1);
}
