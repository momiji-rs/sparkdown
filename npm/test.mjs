// Gate all three entries: `.` (CommonMark), `./gfm` (GFM), `./full` (every
// extension). For each: the init/initSync surface, then the full CommonMark
// 0.31.2 suite must pass 652/652 through the wasm (with extensions off). GFM and
// full extensions are spot-checked.
import * as pure from "./sparkdown.mjs";
import * as gfm from "./gfm.mjs";
import * as full from "./full.mjs";
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
const fullEx = checkInit(full, "full");
if (pureEx === gfmEx || pureEx === fullEx || gfmEx === fullEx) {
  fail("`.`, `./gfm`, `./full` must be separate wasm instances");
}
await pure.ready;
await gfm.ready;
await full.ready;

const dir = dirname(fileURLToPath(import.meta.url));
const spec = JSON.parse(readFileSync(join(dir, "..", "tests", "fixtures", "spec.json"), "utf8"));
const GFM_OFF = { strikethrough: false, tasklist: false, autolink: false, tagfilter: false, tables: false, hardWraps: false };
const FULL_OFF = {
  ...GFM_OFF,
  footnotes: false, emoji: false, diagram: false, frontmatter: false, deflist: false,
  directives: false, headingIds: false, externalLinks: false,
};
let pp = 0;
let gp = 0;
let fp = 0;
for (const e of spec) {
  if (pure.toHtmlSync(e.markdown) === e.html) pp++;
  if (gfm.toHtmlSync(e.markdown, GFM_OFF) === e.html) gp++;
  if (full.toHtmlSync(e.markdown, FULL_OFF) === e.html) fp++;
}

const gfmChecks = [
  ["~~x~~", "<p><del>x</del></p>\n"],
  ["www.example.com", '<p><a href="http://www.example.com">www.example.com</a></p>\n'],
  ["- [x] done", '<ul>\n<li><input checked="" disabled="" type="checkbox"> done</li>\n</ul>\n'],
];
for (const [md, want] of gfmChecks) {
  const got = gfm.toHtmlSync(md);
  if (got !== want) fail(`GFM check ${JSON.stringify(md)}\n  want ${JSON.stringify(want)}\n  got  ${JSON.stringify(got)}`);
  // full includes GFM, so its defaults must produce the same.
  if (full.toHtmlSync(md) !== want) fail(`full(GFM) check ${JSON.stringify(md)} → ${JSON.stringify(full.toHtmlSync(md))}`);
}
// full-only: emoji is on by default.
const emoji = full.toHtmlSync(":tada:");
if (emoji !== "<p>🎉</p>\n") fail(`full emoji check: ":tada:" → ${JSON.stringify(emoji)}`);

console.log(
  `init/initSync ok (3 entries); CommonMark 0.31.2: pure ${pp}/${spec.length}, gfm ${gp}/${spec.length}, full ${fp}/${spec.length}; GFM + full checks ok`,
);
if (pp !== spec.length || gp !== spec.length || fp !== spec.length) process.exit(1);
