// PROTOTYPE: built-in (Rust) heading-id transform vs the JS-plugin route.
//
// Proves the "build common transforms into the wasm render walk" thesis:
//   1. correctness — sparkdown's built-in ids match rehype-slug's (github-slugger)
//   2. speed — "md→HTML + heading ids" all in wasm beats Sätteri doing the same
//      task via a JS plugin (Sätteri has no built-in auto-slug), and crushes the
//      plain-object fallback path.

import { readFileSync } from 'node:fs';
import { unified } from 'unified';
import { visit } from 'unist-util-visit';
import remarkParse from 'remark-parse';
import remarkRehype from 'remark-rehype';
import rehypeSlug from 'rehype-slug';
import rehypeStringify from 'rehype-stringify';
import { markdownToHtml } from 'satteri';
import { parseToMdastWire } from './sparkdown.mjs';
import { mdastToHtml } from './fused_stringify.mjs';
import GitHubSlugger from 'github-slugger';

const x = new WebAssembly.Instance(new WebAssembly.Module(readFileSync(new URL('./sparkdown.wasm', import.meta.url))), {}).exports;
const enc = new TextEncoder();
const dec = new TextDecoder();
const HEADING_IDS = 128; // flag bit 7

function sparkOpts(md, flags) {
  const b = enc.encode(md);
  const ip = x.sparkdown_alloc(b.length);
  new Uint8Array(x.memory.buffer).set(b, ip);
  const p = x.sparkdown_to_html_opts(ip, b.length, flags);
  const n = new DataView(x.memory.buffer).getUint32(p, true);
  const h = dec.decode(new Uint8Array(x.memory.buffer, p + 4, n));
  x.sparkdown_free(p, 4 + n);
  x.sparkdown_free(ip, b.length);
  return h;
}

const MD = readFileSync(new URL('../tests/fixtures/data.md', import.meta.url), 'utf8');
const ids = (html) => [...html.matchAll(/<h[1-6][^>]*\sid="([^"]*)"/g)].map((m) => m[1]);

// ---- 1. correctness: sparkdown built-in ids vs rehype-slug -------------------
const slugRef = unified().use(remarkParse).use(remarkRehype).use(rehypeSlug).use(rehypeStringify);
const refIds = ids(String(slugRef.processSync(MD)));
const ourIds = ids(sparkOpts(MD, HEADING_IDS));
let match = 0;
const mism = [];
for (let i = 0; i < Math.max(refIds.length, ourIds.length); i++) {
  if (refIds[i] === ourIds[i]) match++;
  else if (mism.length < 8) mism.push({ i, ours: ourIds[i], ref: refIds[i] });
}
console.log('\n=== correctness: built-in heading ids vs rehype-slug (github-slugger) ===');
console.log(`  headings: ours ${ourIds.length}, rehype-slug ${refIds.length}`);
console.log(`  identical ids: ${match}/${refIds.length} ${match === refIds.length && ourIds.length === refIds.length ? '✅' : '⚠️'}`);
for (const m of mism) console.log(`    #${m.i}: ours=${JSON.stringify(m.ours)} ref=${JSON.stringify(m.ref)}`);

// ---- 2. speed: same task (md -> HTML with heading ids) -----------------------
const best = (fn, it, tr = 15) => {
  for (let i = 0; i < Math.min(20, it); i++) fn();
  let b = Infinity;
  for (let t = 0; t < tr; t++) { const s = performance.now(); for (let i = 0; i < it; i++) fn(); b = Math.min(b, (performance.now() - s) / it); }
  return b;
};
const F = { features: { gfm: false, frontmatter: false } };
// Sätteri auto-slug as a JS mdast plugin (it has no built-in), via github-slugger
const satSlug = {
  name: 'slug',
  heading(n) {
    const sl = new GitHubSlugger();
    let t = '';
    (function txt(x) { if (x.value) t += x.value; if (x.children) x.children.forEach(txt); })(n);
    return { ...n, data: { ...(n.data || {}), hProperties: { id: sl.slug(t) } } };
  },
};
// sparkdown fallback (plain-object compat path): wire + JS rehype-slug-style + fused
const jsSlug = (tree) => {
  const sl = new GitHubSlugger();
  visit(tree, 'heading', (n) => {
    let t = '';
    (function txt(x) { if (x.value) t += x.value; if (x.children) x.children.forEach(txt); })(n);
    n.data = n.data || {};
    n.data.hProperties = { id: sl.slug(t) };
  });
  return tree;
};

const tBuiltin = best(() => sparkOpts(MD, HEADING_IDS), 100);
const tSat = best(() => markdownToHtml(MD, { ...F, mdastPlugins: [satSlug] }).html, 50);
const tFallback = best(() => mdastToHtml(jsSlug(parseToMdastWire(MD))), 50);
const tSatBase = best(() => markdownToHtml(MD, F).html, 50);

console.log('\n=== speed: md -> HTML WITH heading ids (198 KB), best-of-15 ===\n');
const lo = Math.min(tBuiltin, tSat, tFallback);
const w = (l, ms) => console.log(`  ${l.padEnd(48)} ${ms.toFixed(2).padStart(7)} ms   ${(ms / lo).toFixed(2)}x`);
w('sparkdown BUILT-IN (all wasm) ★', tBuiltin);
w('satteri + JS slug plugin', tSat);
w('sparkdown fallback (wire + JS slug + fused)', tFallback);
console.log(`\n  (ref: satteri no transform = ${tSatBase.toFixed(2)} ms)`);
console.log(`  built-in vs satteri:  ${(tSat / tBuiltin).toFixed(2)}× faster`);
