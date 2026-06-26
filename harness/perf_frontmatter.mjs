// Perf verification (task #8): native frontmatter on a frontmatter-prefixed doc.
//
// Goals for a native grammar extension:
//   (a) enabling it stays on the all-wasm fast path (≈ plain to_html),
//   (b) it beats Sätteri on the same task,
//   (c) it beats the plain-object fallback (wire + JS + fused stringify).
//
// Run: node perf_frontmatter.mjs

import { readFileSync } from 'node:fs';
import { markdownToHtml } from 'satteri';
import { best, report, sparkOpts, sparkToHtml } from './perf_harness.mjs';
import { parseToMdastWire } from './sparkdown.mjs';
import { mdastToHtml } from './fused_stringify.mjs';

const FRONTMATTER = 256; // flag bit 8

const BODY = readFileSync(new URL('../tests/fixtures/data.md', import.meta.url), 'utf8');
const FM = '---\ntitle: Benchmark Document\nauthor: sparkdown\ntags:\n  - perf\n  - frontmatter\ndate: 2026-06-25\n---\n\n';
const DOC = FM + BODY;
const SAT = { features: { gfm: false, frontmatter: true } };

report('frontmatter doc → HTML (≈198 KB), best-of-15', [
  { name: 'sparkdown built-in (all wasm)', ms: best(() => sparkOpts(DOC, FRONTMATTER)) },
  { name: 'satteri (frontmatter on)', ms: best(() => markdownToHtml(DOC, SAT).html, { iters: 50 }) },
  { name: 'sparkdown fallback (wire+JS+fused)', ms: best(() => mdastToHtml(parseToMdastWire(DOC, FRONTMATTER)), { iters: 50 }) },
]);

// Overhead check: the same body with the frontmatter grammar enabled vs the plain
// CommonMark fast path on the bare body. Enabling the extension must be ~free.
const tBare = best(() => sparkToHtml(BODY));
const tFm = best(() => sparkOpts(DOC, FRONTMATTER));
console.log(`\n  fast-path overhead: bare to_html ${tBare.toFixed(2)} ms vs +frontmatter ${tFm.toFixed(2)} ms ` +
  `(${((tFm / tBare - 1) * 100).toFixed(1)}%)`);
