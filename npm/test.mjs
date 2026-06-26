// Gate all three entries: `.` (CommonMark), `./gfm` (GFM), `./full` (every
// extension). For each: the init/initSync surface, then the full CommonMark
// 0.31.2 suite must pass 652/652 through the wasm (with extensions off). GFM and
// full extensions are spot-checked.
import * as pure from "./sparkdown.mjs";
import * as gfm from "./gfm.mjs";
import * as full from "./full.mjs";
import * as mdast from "./mdast.mjs";
import sparkdownParse from "./mdast.mjs";
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
const mdastEx = checkInit(mdast, "mdast"); // toHtmlSync("# hi") agrees with the others on simple input
if (new Set([pureEx, gfmEx, fullEx, mdastEx]).size !== 4) {
  fail("`.`, `./gfm`, `./full`, `./mdast` must be separate wasm instances");
}
await pure.ready;
await gfm.ready;
await full.ready;
await mdast.ready;

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

// `./mdast` — the parser entry. The full deep-equal vs mdast-util-from-markdown and
// the unified-pipeline parity (652/652) run in the harness (ecosystem deps); here,
// self-contained: every spec example parses to a valid mdast root, plus shape spot
// checks and the unified Parser plugin. (toHtmlSync emits the *unified* HTML shape,
// not cmark's, so it is not gated against spec.html.)
let mp = 0;
for (const e of spec) {
  const t = mdast.toMdastSync(e.markdown);
  if (t && t.type === "root" && Array.isArray(t.children)) mp++;
}
if (mp !== spec.length) fail(`mdast: toMdastSync produced ${mp}/${spec.length} valid roots`);

const h1 = mdast.toMdastSync("# hi");
if (h1.children[0]?.type !== "heading" || h1.children[0].depth !== 1 || h1.children[0].children[0]?.value !== "hi") {
  fail(`mdast: toMdastSync("# hi") shape → ${JSON.stringify(h1.children[0])}`);
}
if (mdast.toHtmlSync("# *hi*\n") !== "<h1><em>hi</em></h1>\n") fail("mdast: toHtmlSync render");
if (typeof sparkdownParse !== "function") fail("mdast: default export must be the unified Parser plugin");
// The plugin installs `this.parser`; emulate unified attaching it.
const ctx = {};
sparkdownParse.call(ctx);
if (typeof ctx.parser !== "function" || ctx.parser("# hi").type !== "root") fail("mdast: sparkdownParse plugin");

console.log(
  `init/initSync ok (4 entries); CommonMark 0.31.2: pure ${pp}/${spec.length}, gfm ${gp}/${spec.length}, full ${fp}/${spec.length}; mdast roots ${mp}/${spec.length}; GFM + full + mdast checks ok`,
);
if (pp !== spec.length || gp !== spec.length || fp !== spec.length) process.exit(1);
