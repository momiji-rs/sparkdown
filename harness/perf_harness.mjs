// Reusable PERF-VERIFICATION harness (task #7).
//
// Each native extension must (a) stay on the all-wasm fast path (≈ to_html
// speed), (b) beat Sätteri on the same task, (c) beat the plain-object fallback.
// This generalizes the bench in builtin_heading_ids.mjs so each extension's perf
// file is a few lines:
//
//   import { best, report, wasm, sparkOpts } from './perf_harness.mjs';
//   report('feature X — md→HTML', [
//     { name: 'sparkdown built-in (wasm)', ms: best(() => sparkOpts(MD, FLAG)) },
//     { name: 'satteri', ms: best(() => satteri.markdownToHtml(MD, opts).html) },
//     { name: 'fallback', ms: best(() => fallback(MD)) },
//   ]);
//
// Run directly (`node perf_harness.mjs`) for a self-test reproducing the
// heading-id comparison through this harness.

import { readFileSync } from 'node:fs';
import { pathToFileURL } from 'node:url';

/** Best-of-N min estimator (defeats GC/JIT noise). Returns ms/op. */
export function best(fn, { iters = 100, trials = 15, warmup = 20 } = {}) {
  for (let i = 0; i < Math.min(warmup, iters); i++) fn();
  let b = Infinity;
  for (let t = 0; t < trials; t++) {
    const s = performance.now();
    for (let i = 0; i < iters; i++) fn();
    b = Math.min(b, (performance.now() - s) / iters);
  }
  return b;
}

/** Print a labelled table; `rows` = [{name, ms}]. Marks the fastest, shows ×slowdown. */
export function report(label, rows) {
  const lo = Math.min(...rows.map((r) => r.ms));
  console.log(`\n=== ${label} ===\n`);
  for (const r of rows.slice().sort((a, b) => a.ms - b.ms)) {
    const star = r.ms === lo ? ' ★' : '';
    console.log(`  ${r.name.padEnd(46)} ${r.ms.toFixed(2).padStart(7)} ms   ${(r.ms / lo).toFixed(2)}x${star}`);
  }
  return { fastest: lo, rows };
}

/** The shared sparkdown wasm instance (built with gfm,ast). */
export const wasm = new WebAssembly.Instance(
  new WebAssembly.Module(readFileSync(new URL('./sparkdown.wasm', import.meta.url))),
  {},
).exports;

const enc = new TextEncoder();
const dec = new TextDecoder();

/** Render via `sparkdown_to_html_opts(flags)` — the all-wasm built-in path. */
export function sparkOpts(md, flags = 0) {
  const b = enc.encode(md);
  const ip = wasm.sparkdown_alloc(b.length);
  new Uint8Array(wasm.memory.buffer).set(b, ip);
  const p = wasm.sparkdown_to_html_opts(ip, b.length, flags);
  const n = new DataView(wasm.memory.buffer).getUint32(p, true);
  const h = dec.decode(new Uint8Array(wasm.memory.buffer, p + 4, n));
  wasm.sparkdown_free(p, 4 + n);
  wasm.sparkdown_free(ip, b.length);
  return h;
}

/** Plain CommonMark render via `sparkdown_to_html` (no options) — the fast-path floor. */
export function sparkToHtml(md) {
  const b = enc.encode(md);
  const ip = wasm.sparkdown_alloc(b.length);
  new Uint8Array(wasm.memory.buffer).set(b, ip);
  const p = wasm.sparkdown_to_html(ip, b.length);
  const n = new DataView(wasm.memory.buffer).getUint32(p, true);
  const h = dec.decode(new Uint8Array(wasm.memory.buffer, p + 4, n));
  wasm.sparkdown_free(p, 4 + n);
  wasm.sparkdown_free(ip, b.length);
  return h;
}

// --- self-test: reproduce the heading-id perf comparison through this harness -
if (import.meta.url === pathToFileURL(process.argv[1] || '').href) {
  const { markdownToHtml } = await import('satteri');
  const { visit } = await import('unist-util-visit');
  const { parseToMdastWire } = await import('./sparkdown.mjs');
  const { mdastToHtml } = await import('./fused_stringify.mjs');
  const GitHubSlugger = (await import('github-slugger')).default;

  const MD = readFileSync(new URL('../tests/fixtures/data.md', import.meta.url), 'utf8');
  const F = { features: { gfm: false, frontmatter: false } };
  const HEADING_IDS = 128;

  const satSlug = {
    name: 'slug',
    heading(n) {
      const sl = new GitHubSlugger();
      let t = '';
      (function txt(z) { if (z.value) t += z.value; if (z.children) z.children.forEach(txt); })(n);
      return { ...n, data: { ...(n.data || {}), hProperties: { id: sl.slug(t) } } };
    },
  };
  const jsSlug = (tree) => {
    const sl = new GitHubSlugger();
    visit(tree, 'heading', (n) => {
      let t = '';
      (function txt(z) { if (z.value) t += z.value; if (z.children) z.children.forEach(txt); })(n);
      n.data = n.data || {};
      n.data.hProperties = { id: sl.slug(t) };
    });
    return tree;
  };

  report('self-test: md→HTML + heading ids (198 KB)', [
    { name: 'sparkdown built-in (all wasm)', ms: best(() => sparkOpts(MD, HEADING_IDS)) },
    { name: 'satteri + JS slug plugin', ms: best(() => markdownToHtml(MD, { ...F, mdastPlugins: [satSlug] }).html, { iters: 50 }) },
    { name: 'sparkdown fallback (wire+JS+fused)', ms: best(() => mdastToHtml(jsSlug(parseToMdastWire(MD))), { iters: 50 }) },
  ]);
}
