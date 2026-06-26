// End-to-end task benchmark: sparkdown's combinations vs Sätteri vs pure-JS
// remark — "given a markdown string, accomplish the same task, how long?".
//
// Three scenarios, each a real task you'd actually run:
//
//   A. markdown -> HTML, no plugins        raw engine throughput
//   B. markdown -> HTML, with a plugin     the realistic ecosystem task
//   C. markdown -> mdast tree in JS        the architectural crux: how dear is
//                                          it to get a usable AST into JS?
//
// Combinations (each present only where the task allows):
//   remark (pure JS)        unified: remark-parse -> remark-rehype -> rehype-stringify
//   satteri (Rust napi)     markdownToHtml / markdownToMdast — AST lives in a Rust arena;
//                           plugins run in-arena (no full-tree copy unless materialized)
//   sparkdown -> unified    unified: sparkdown(wasm; mdast copied to JS as JSON) -> rehype
//   sparkdown to_html (wasm) direct Rust md->HTML, no AST, no JS pipeline (the floor)
//
// All CommonMark (GFM off) so the work is identical. Processors are built ONCE
// (you build a pipeline, then process many docs). best-of-5, iters auto-scaled.

import { readFileSync } from 'node:fs';
import { unified } from 'unified';
import { visit } from 'unist-util-visit';
import remarkParse from 'remark-parse';
import remarkRehype from 'remark-rehype';
import rehypeStringify from 'rehype-stringify';
import { markdownToHtml, markdownToMdast, defineHastPlugin } from 'satteri';
import sparkdown, { parseToMdast, parseToMdastWire } from './sparkdown.mjs';

// --- sparkdown direct wasm to_html (no mdast) -------------------------------
const wasmBytes = readFileSync(new URL('./sparkdown.wasm', import.meta.url));
const wx = new WebAssembly.Instance(new WebAssembly.Module(wasmBytes), {}).exports;
const enc = new TextEncoder();
const dec = new TextDecoder();
function sparkdownToHtml(md) {
  const buf = enc.encode(md);
  const inPtr = wx.sparkdown_alloc(buf.length);
  new Uint8Array(wx.memory.buffer).set(buf, inPtr);
  const ptr = wx.sparkdown_to_html(inPtr, buf.length);
  const len = new DataView(wx.memory.buffer).getUint32(ptr, true);
  const html = dec.decode(new Uint8Array(wx.memory.buffer, ptr + 4, len));
  wx.sparkdown_free(ptr, 4 + len);
  wx.sparkdown_free(inPtr, buf.length);
  return html;
}

const satteriFeat = { features: { gfm: false, frontmatter: false } };
// Sätteri backend: native napi by default, or its wasm (wasi) fallback when
// NAPI_RS_FORCE_WASI=1 — the apples-to-apples comparison vs sparkdown's wasm.
const sat = process.env.NAPI_RS_FORCE_WASI ? 'satteri (wasi/wasm)' : 'satteri (Rust napi)';

// --- plugins: a read-only visitor that touches every node (noop) ------------
// remark/unified flavour: walk the whole mdast.
function remarkNoop() {
  return (tree) => {
    let n = 0;
    visit(tree, () => {
      n++;
    });
    return tree;
  };
}
// satteri flavour: per-node hast visitor (its own bench's noop shape).
const satteriNoop = defineHastPlugin({
  name: 'noop',
  createOnce: () => ({
    element() {},
    text() {},
  }),
});

// --- processors built once --------------------------------------------------
const rehype = (p) =>
  p.use(remarkRehype, { allowDangerousHtml: true }).use(rehypeStringify, { allowDangerousHtml: true });

const remarkHtml = rehype(unified().use(remarkParse));
const remarkHtmlP = rehype(unified().use(remarkParse).use(remarkNoop));
const sparkHtml = rehype(unified().use(sparkdown));
const sparkHtmlP = rehype(unified().use(sparkdown).use(remarkNoop));
const remarkParseOnly = unified().use(remarkParse); // .parse() = mdast, no transforms

// --- timing -----------------------------------------------------------------
function measure(fn, budgetMs = 1500) {
  fn();
  const t0 = performance.now();
  fn();
  const one = performance.now() - t0 || 0.001;
  const iters = Math.min(20000, Math.max(20, Math.round(budgetMs / one)));
  const warm = Math.max(10, Math.round(iters / 10));
  for (let i = 0; i < warm; i++) fn();
  let best = Infinity;
  for (let b = 0; b < 5; b++) {
    const s = performance.now();
    for (let i = 0; i < iters; i++) fn();
    best = Math.min(best, (performance.now() - s) / iters);
  }
  return best; // ms/op
}

function table(title, bytes, rows) {
  const fastest = Math.min(...rows.map((r) => r.ms));
  console.log(`\n--- ${title} ---\n`);
  console.log(
    `  ${'combination'.padEnd(32)} ${'ms/op'.padStart(9)} ${'ops/s'.padStart(9)} ${'MB/s'.padStart(7)} ${'vs best'.padStart(9)}  ${'size'}`,
  );
  console.log(`  ${'-'.repeat(32)} ${'-'.repeat(9)} ${'-'.repeat(9)} ${'-'.repeat(7)} ${'-'.repeat(9)}  ----`);
  for (const r of rows) {
    const mbps = bytes / 1024 / 1024 / (r.ms / 1000);
    console.log(
      `  ${r.name.padEnd(32)} ${r.ms.toFixed(4).padStart(9)} ${(1000 / r.ms).toFixed(0).padStart(9)} ${mbps.toFixed(0).padStart(7)} ${(r.ms / fastest).toFixed(2).padStart(8)}x  ${r.size}`,
    );
  }
}

function row(name, fn, sizeOf) {
  const out = fn();
  return { name, ms: measure(fn), size: sizeOf(out) };
}
const kb = (s) => `${(s.length / 1024).toFixed(1)}KB html`;
const nodes = (t) => `${countNodes(t)} nodes`;
function countNodes(t) {
  let n = 1;
  for (const c of t.children || []) n += countNodes(c);
  return n;
}

function suite(title, md) {
  const bytes = Buffer.byteLength(md, 'utf8');
  console.log(`\n========== ${title} (${(bytes / 1024).toFixed(1)} KB markdown) ==========`);

  table('A. markdown -> HTML (no plugins)', bytes, [
    row('remark (pure JS)', () => String(remarkHtml.processSync(md)), kb),
    row(sat, () => markdownToHtml(md, satteriFeat).html, kb),
    row('sparkdown -> unified (wasm+JS)', () => String(sparkHtml.processSync(md)), kb),
    row('sparkdown to_html (wasm)', () => sparkdownToHtml(md), kb),
  ]);

  table('B. markdown -> HTML (with a per-node plugin)', bytes, [
    row('remark + noop visitor', () => String(remarkHtmlP.processSync(md)), kb),
    row(`${sat} + noop plugin`, () => markdownToHtml(md, { ...satteriFeat, hastPlugins: [satteriNoop] }).html, kb),
    row('sparkdown -> unified + noop', () => String(sparkHtmlP.processSync(md)), kb),
  ]);

  // C: a *fully usable* JS tree — every node reachable (what any visitor/plugin
  // or stringify needs). Force a full walk so lazy trees pay their real cost;
  // eager trees (JSON.parse) just pay a cheap extra traversal. Apples to apples.
  const full = (parse) => {
    const fn = () => countNodes(parse(md));
    const n = fn();
    return { ms: measure(fn), n };
  };
  const remarkFull = full((m) => remarkParseOnly.parse(m));
  const satFull = full((m) => markdownToMdast(m, { features: satteriFeat.features }));
  const sparkJsonFull = full((m) => parseToMdast(m));
  const sparkWireFull = full((m) => parseToMdastWire(m));
  table('C. markdown -> FULLY MATERIALIZED mdast in JS (every node reachable)', bytes, [
    { name: 'remark-parse (native JS tree)', ms: remarkFull.ms, size: `${remarkFull.n} nodes` },
    { name: `${sat} markdownToMdast`, ms: satFull.ms, size: `${satFull.n} nodes` },
    { name: 'sparkdown wasm -> JSON -> JS', ms: sparkJsonFull.ms, size: `${sparkJsonFull.n} nodes` },
    { name: 'sparkdown wasm -> WIRE -> JS  ★', ms: sparkWireFull.ms, size: `${sparkWireFull.n} nodes` },
  ]);

  // C': the lazy advantage — Sätteri's tree before anything walks it. sparkdown's
  // boundary can't do partial: it always materializes the whole tree.
  table("C'. markdown -> mdast HANDLE / shell (before touching nodes)", bytes, [
    { name: `${sat} markdownToMdast (lazy shell)`, ms: measure(() => markdownToMdast(md, { features: satteriFeat.features })), size: 'lazy' },
    { name: 'sparkdown wasm -> JSON -> JS (eager)', ms: measure(() => parseToMdast(md)), size: 'full' },
    { name: 'sparkdown wasm -> WIRE -> JS (eager) ★', ms: measure(() => parseToMdastWire(md)), size: 'full' },
  ]);
}

const SMALL = `# Getting started

Welcome to **sparkdown** — a fast CommonMark parser. Install it and call
\`to_html\`. It supports *emphasis*, \`inline code\`, [links](https://example.com),
and the usual block structure.

## Features

- byte-identical to cmark
- zero dependencies
- optional GFM and a programmable mdast

> A blockquote, because every README has one.

\`\`\`rust
fn main() { println!("hello"); }
\`\`\`

1. parse
2. transform
3. render

See the [docs](https://example.com/docs) for more.
`;
const LARGE = readFileSync(new URL('../tests/fixtures/data.md', import.meta.url), 'utf8');

console.log('\nEnd-to-end: same task, sparkdown combos vs Sätteri vs remark. CommonMark, GFM off.');
console.log('best-of-5, iterations auto-scaled to ~1.5s/pipeline. Lower ms/op = faster.');
suite('small realistic doc', SMALL);
suite('CommonMark spec', LARGE);
console.log();
