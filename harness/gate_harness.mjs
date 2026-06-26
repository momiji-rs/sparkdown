// Reusable ALIGNMENT-GATE harness (task #6).
//
// Every native sparkdown extension must produce output that matches its
// established ecosystem equivalent. This module generalizes the bespoke check in
// builtin_heading_ids.mjs so each new extension's gate is a few lines:
//
//   import { runGate, loadCorpus, eqMdast } from './gate_harness.mjs';
//   runGate({ label: 'frontmatter vs remark-frontmatter', items: loadCorpus(),
//     a: (md) => sparkdownMdast(md),            // ours (native)
//     b: (md) => remarkFrontmatterTree(md),     // the reference plugin
//     eq: eqMdast({ dropPos: true }) });
//
// Two comparison modes:
//   - transform-type: `a`/`b` return the relevant slice (rendered HTML, an id
//     list, link attrs, emoji output…) and `eq` is the default JSON equality.
//   - grammar-type:   `a`/`b` return mdast trees and `eq` is `eqMdast(...)`,
//     which canonicalizes (sorted keys, dropped `undefined`, optional position).
//
// Run directly (`node gate_harness.mjs`) for a self-test: it reproduces the
// heading-id ↔ rehype-slug gate THROUGH this harness over the full 652 corpus.

import { readFileSync } from 'node:fs';
import { pathToFileURL } from 'node:url';

/** The 652 CommonMark examples as `{ example, markdown }` (built mdast tree too). */
export function loadCorpus() {
  const data = JSON.parse(readFileSync(new URL('./sparkdown-mdast.json', import.meta.url), 'utf8'));
  return data.map((e) => ({ example: e.example, markdown: e.markdown }));
}

/** Canonical JSON of a unist/mdast tree: keys sorted, `undefined` dropped,
 *  `position` optionally dropped — so two trees compare structurally. */
export function canon(node, { dropPos = false } = {}) {
  const walk = (n) => {
    if (Array.isArray(n)) return n.map(walk);
    if (n && typeof n === 'object') {
      const o = {};
      for (const k of Object.keys(n).sort()) {
        if (dropPos && k === 'position') continue;
        if (n[k] === undefined) continue;
        o[k] = walk(n[k]);
      }
      return o;
    }
    return n;
  };
  return JSON.stringify(walk(node));
}

/** Default equality: deep JSON. Works for strings, arrays (e.g. id lists), objects. */
export const eqJson = (x, y) => JSON.stringify(x) === JSON.stringify(y);

/** mdast/unist deep-equal with canonicalization. `eqMdast({ dropPos: true })`. */
export const eqMdast =
  (opts = {}) =>
  (x, y) =>
    canon(x, opts) === canon(y, opts);

/**
 * Run an alignment gate. `a(md)` = ours (native), `b(md)` = the reference plugin.
 * Returns { pass, total, ok, mismatches } and prints a one-line verdict + samples.
 */
export function runGate({ label, items, a, b, eq = eqJson, limit = 8, quiet = false }) {
  let pass = 0;
  const mism = [];
  for (const it of items) {
    const md = typeof it === 'string' ? it : it.markdown;
    let av, bv;
    try { av = a(md); } catch (e) { av = { __err: String(e) }; }
    try { bv = b(md); } catch (e) { bv = { __err: String(e) }; }
    if (eq(av, bv)) pass++;
    else if (mism.length < limit) mism.push({ id: (typeof it === 'object' && it.example) ?? md.slice(0, 40), a: av, b: bv });
  }
  const ok = pass === items.length;
  if (!quiet) {
    console.log(`gate ${label}: ${pass}/${items.length} ${ok ? '✅' : '❌'}`);
    for (const m of mism) {
      console.log(`  #${m.id}`);
      console.log(`    ours: ${JSON.stringify(m.a).slice(0, 140)}`);
      console.log(`    ref : ${JSON.stringify(m.b).slice(0, 140)}`);
    }
  }
  return { pass, total: items.length, ok, mismatches: mism };
}

// --- self-test: reproduce the heading-id gate through this harness -----------
if (import.meta.url === pathToFileURL(process.argv[1] || '').href) {
  const { unified } = await import('unified');
  const remarkParse = (await import('remark-parse')).default;
  const remarkRehype = (await import('remark-rehype')).default;
  const rehypeSlug = (await import('rehype-slug')).default;
  const rehypeStringify = (await import('rehype-stringify')).default;

  const x = new WebAssembly.Instance(new WebAssembly.Module(readFileSync(new URL('./sparkdown.wasm', import.meta.url))), {}).exports;
  const enc = new TextEncoder();
  const dec = new TextDecoder();
  const HEADING_IDS = 128;
  const sparkOpts = (md, flags) => {
    const b = enc.encode(md);
    const ip = x.sparkdown_alloc(b.length);
    new Uint8Array(x.memory.buffer).set(b, ip);
    const p = x.sparkdown_to_html_opts(ip, b.length, flags);
    const n = new DataView(x.memory.buffer).getUint32(p, true);
    const h = dec.decode(new Uint8Array(x.memory.buffer, p + 4, n));
    x.sparkdown_free(p, 4 + n);
    x.sparkdown_free(ip, b.length);
    return h;
  };
  const ids = (html) => [...html.matchAll(/<h[1-6][^>]*\sid="([^"]*)"/g)].map((m) => m[1]);
  const slugRef = unified().use(remarkParse).use(remarkRehype).use(rehypeSlug).use(rehypeStringify);

  const items = loadCorpus();
  console.log('self-test over the full 652 corpus:\n');

  // (a) transform mode — built-in heading ids vs rehype-slug (extracted id lists).
  const r1 = runGate({
    label: 'transform: heading-ids vs rehype-slug',
    items,
    a: (md) => ids(sparkOpts(md, HEADING_IDS)),
    b: (md) => ids(String(slugRef.processSync(md))),
  });

  // (b) grammar mode — sparkdown's mdast vs remark-parse's, via eqMdast (deep-equal
  // ignoring position). The exact shape every grammar extension's gate will use.
  const { parseToMdastWire } = await import('./sparkdown.mjs');
  const remarkTree = unified().use(remarkParse);
  const r2 = runGate({
    label: 'grammar: sparkdown mdast vs remark-parse',
    items,
    a: (md) => parseToMdastWire(md),
    b: (md) => remarkTree.parse(md),
    eq: eqMdast({ dropPos: true }),
  });

  process.exit(r1.ok && r2.ok ? 0 : 1);
}
