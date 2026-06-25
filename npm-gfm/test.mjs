// Gate: (1) CommonMark 0.31.2 — all 652 examples, with every GFM flag OFF, must
// still match (the engine is the same); (2) GFM spot checks with the defaults.
import { toHtmlSync, ready } from "./sparkdown.mjs";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const dir = dirname(fileURLToPath(import.meta.url));
const spec = JSON.parse(readFileSync(join(dir, "..", "tests", "fixtures", "spec.json"), "utf8"));

await ready;
const PURE = { strikethrough: false, tasklist: false, autolink: false, tagfilter: false, tables: false, hardWraps: false };
let pass = 0;
const failed = [];
for (const ex of spec) {
  if (toHtmlSync(ex.markdown, PURE) === ex.html) pass++;
  else failed.push(ex.example);
}

const checks = [
  ["~~x~~", "<p><del>x</del></p>\n"],
  ["www.example.com", '<p><a href="http://www.example.com">www.example.com</a></p>\n'],
  ["- [x] done", '<ul>\n<li><input checked="" disabled="" type="checkbox"> done</li>\n</ul>\n'],
];
for (const [md, want] of checks) {
  const got = toHtmlSync(md);
  if (got !== want) {
    console.error(`GFM check failed: ${JSON.stringify(md)}\n  want ${JSON.stringify(want)}\n  got  ${JSON.stringify(got)}`);
    process.exit(1);
  }
}

console.log(`CommonMark 0.31.2 via wasm (gfm flags off): ${pass}/${spec.length}; GFM spot checks ok`);
if (pass !== spec.length) {
  console.error("FAILED examples:", failed.slice(0, 15));
  process.exit(1);
}
