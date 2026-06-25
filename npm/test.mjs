// Gate both entries of the package: the `.` (CommonMark) and `./gfm` (GFM)
// builds. For each: the init/initSync surface, then the full CommonMark 0.31.2
// suite must pass 652/652 through the wasm. GFM extensions are spot-checked.
import * as pure from "./sparkdown.mjs";
import * as gfm from "./gfm.mjs";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const fail = (m) => {
  console.error("FAIL:", m);
  process.exit(1);
};

// Each entry is an independent module/singleton, lazily instantiated.
function checkInit(mod, name) {
  let threw = "";
  try {
    mod.toHtmlSync("# hi");
  } catch (e) {
    threw = e.message;
  }
  if (!/initSync/.test(threw)) fail(`${name}: toHtmlSync before init should throw (got: ${threw})`);
  const ex = mod.initSync();
  if (mod.toHtmlSync("# hi") !== "<h1>hi</h1>\n") fail(`${name}: initSync()+toHtmlSync`);
  if (mod.initSync() !== ex) fail(`${name}: initSync() not idempotent`);
  return ex;
}
const pureEx = checkInit(pure, "pure");
const gfmEx = checkInit(gfm, "gfm");
if (pureEx === gfmEx) fail("`.` and `./gfm` must be separate wasm instances");
await pure.ready;
await gfm.ready;

const dir = dirname(fileURLToPath(import.meta.url));
const spec = JSON.parse(readFileSync(join(dir, "..", "tests", "fixtures", "spec.json"), "utf8"));
const PURE_OPTS = { strikethrough: false, tasklist: false, autolink: false, tagfilter: false, tables: false, hardWraps: false };
let pp = 0;
let gp = 0;
for (const e of spec) {
  if (pure.toHtmlSync(e.markdown) === e.html) pp++;
  if (gfm.toHtmlSync(e.markdown, PURE_OPTS) === e.html) gp++;
}

const checks = [
  ["~~x~~", "<p><del>x</del></p>\n"],
  ["www.example.com", '<p><a href="http://www.example.com">www.example.com</a></p>\n'],
  ["- [x] done", '<ul>\n<li><input checked="" disabled="" type="checkbox"> done</li>\n</ul>\n'],
];
for (const [md, want] of checks) {
  const got = gfm.toHtmlSync(md);
  if (got !== want) fail(`GFM check ${JSON.stringify(md)}\n  want ${JSON.stringify(want)}\n  got  ${JSON.stringify(got)}`);
}

console.log(`init/initSync ok (both entries); CommonMark 0.31.2: pure ${pp}/${spec.length}, gfm ${gp}/${spec.length}; GFM checks ok`);
if (pp !== spec.length || gp !== spec.length) process.exit(1);
