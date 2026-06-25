// End-to-end with a REAL transformer + a per-STAGE breakdown.
//
// "Is the wire parser enough?" depends entirely on what fraction of the whole
// task the parse actually is. This runs the realistic unified task —
//   markdown -> mdast -> [transform plugins] -> hast -> HTML
// — and times each stage separately, so we can SEE whether the #1 parse matters
// end-to-end or gets swamped by the JS-side mdast->hast->stringify tail.
//
// Compares, on the same doc, identical output:
//   sparkdown(WIRE) -> unified plugins -> rehype   vs   remark-parse -> same
// and the whole-task total vs Sätteri (all-Rust pipeline; render never enters JS).

import { readFileSync } from 'node:fs';
import { unified } from 'unified';
import { visit } from 'unist-util-visit';
import remarkParse from 'remark-parse';
import remarkRehype from 'remark-rehype';
import rehypeStringify from 'rehype-stringify';
import { toHast } from 'mdast-util-to-hast';
import { toHtml } from 'hast-util-to-html';
import { markdownToHtml } from 'satteri';
import { parseToMdastWire } from './sparkdown.mjs';

const LARGE = readFileSync(new URL('../tests/fixtures/data.md', import.meta.url), 'utf8');

// --- a REAL transformer: walk every node, mutate proportional to tree size ---
// (representative of real plugin work: heading ids, link attrs, text rewrite.)
function transform(tree) {
  visit(tree, (node) => {
    if (node.type === 'heading') {
      node.data = node.data || {};
      node.data.hProperties = { id: `h-${node.depth}` };
    } else if (node.type === 'link') {
      node.data = node.data || {};
      node.data.hProperties = { rel: 'nofollow', target: '_blank' };
    } else if (node.type === 'text') {
      // touch the value (a real text-rewriting plugin, e.g. smartypants/emoji)
      if (node.value.includes('--')) node.value = node.value.replace(/--/g, '—');
    }
  });
  return tree;
}
// Sätteri equivalent, in ITS plugin API. NOTE the different mutation model:
// the visited node is read-only and a change is expressed by RETURNING a
// replacement node (Sätteri encodes it into a binary command buffer applied in
// Rust) — you cannot mutate in place like a unist visitor. The visitor functions
// are still JS, fired per subscribed node, so this is NOT free-in-Rust work.
const satTransform = {
  name: 'transform',
  heading(n) { return { ...n, data: { ...(n.data || {}), hProperties: { id: `h-${n.depth}` } } }; },
  link(n) { return { ...n, data: { ...(n.data || {}), hProperties: { rel: 'nofollow', target: '_blank' } } }; },
  text(n) { if (n.value.includes('--')) return { ...n, value: n.value.replace(/--/g, '—') }; },
};

function best(fn, iters, trials = 12) {
  for (let i = 0; i < Math.min(20, iters); i++) fn();
  let b = Infinity;
  for (let t = 0; t < trials; t++) {
    const s = performance.now();
    for (let i = 0; i < iters; i++) fn();
    b = Math.min(b, (performance.now() - s) / iters);
  }
  return b;
}

// Verify identical output first (wire+transform vs remark+transform, full pipe).
const wireProc = unified()
  .use(function () { this.parser = (d) => parseToMdastWire(d); })
  .use(() => transform)
  .use(remarkRehype, { allowDangerousHtml: true })
  .use(rehypeStringify, { allowDangerousHtml: true });
const remarkProc = unified()
  .use(remarkParse)
  .use(() => transform)
  .use(remarkRehype, { allowDangerousHtml: true })
  .use(rehypeStringify, { allowDangerousHtml: true });
const sameOut = String(wireProc.processSync(LARGE)) === String(remarkProc.processSync(LARGE));

const bytes = Buffer.byteLength(LARGE, 'utf8');
console.log(`\nEnd-to-end WITH a real transformer — CommonMark spec (${(bytes / 1024).toFixed(0)} KB), best-of-12`);
console.log(`wire+transform output == remark+transform output: ${sameOut ? '✅ identical' : '❌ DIFFERS'}\n`);

// --- per-stage breakdown (the whole point) ----------------------------------
// Stages 2-4 are parser-agnostic (same tree shape), so time them on the wire tree.
const IT = 50;
const tParseWire = best(() => parseToMdastWire(LARGE), IT);
const tParseRemark = best(() => unified().use(remarkParse).parse(LARGE), IT);
const tTransform = best(() => { transform(parseToMdastWire(LARGE)); }, IT) - tParseWire;
const tHast = best(() => { toHast(transform(parseToMdastWire(LARGE)), { allowDangerousHtml: true }); }, IT) - (tParseWire + tTransform);
const tHtml = (() => {
  const h = toHast(transform(parseToMdastWire(LARGE)), { allowDangerousHtml: true });
  return best(() => toHtml(h, { allowDangerousHtml: true }), IT);
})();

const r = (l, ms, tot) => console.log(`  ${l.padEnd(40)} ${ms.toFixed(2).padStart(7)} ms   ${((ms / tot) * 100).toFixed(0).padStart(3)}%`);
const wireTotal = tParseWire + tTransform + tHast + tHtml;
const remarkTotal = tParseRemark + tTransform + tHast + tHtml;

console.log('STAGE BREAKDOWN (sparkdown-wire pipeline):');
r('1. parse      md -> mdast  (wire)', tParseWire, wireTotal);
r('2. transform  visit + mutate every node', tTransform, wireTotal);
r('3. mdast->hast  (mdast-util-to-hast)', tHast, wireTotal);
r('4. hast->HTML   (hast-util-to-html)', tHtml, wireTotal);
console.log(`  ${'-'.repeat(54)}`);
r('   TOTAL (sparkdown wire -> unified)', wireTotal, wireTotal);

console.log('\nFor contrast, swap ONLY stage 1 for remark-parse:');
r('1. parse      md -> mdast  (remark-parse)', tParseRemark, remarkTotal);
r('   TOTAL (remark -> unified, same stages 2-4)', remarkTotal, remarkTotal);

console.log('\nWHOLE TASK md -> HTML (with the same transformer):');
const F = { features: { gfm: false, frontmatter: false } };
// sanity: confirm Sätteri's transform actually applied (else the compare is unfair)
const satOut = markdownToHtml(LARGE, { ...F, mdastPlugins: [satTransform] }).html;
const satApplied = satOut !== markdownToHtml(LARGE, F).html;
const fullWire = best(() => String(wireProc.processSync(LARGE)), 20);
const fullRemark = best(() => String(remarkProc.processSync(LARGE)), 12);
const fullSatBase = best(() => markdownToHtml(LARGE, F).html, 50);
const fullSat = best(() => markdownToHtml(LARGE, { ...F, mdastPlugins: [satTransform] }).html, 50);
console.log(`  (sätteri transform applied: ${satApplied ? '✅' : '❌ NOT — unfair!'})`);
const lo = Math.min(fullWire, fullRemark, fullSat);
const w = (l, ms) => console.log(`  ${l.padEnd(42)} ${ms.toFixed(2).padStart(7)} ms   ${(ms / lo).toFixed(2)}x`);
w('satteri (parse+transform+hast+render)', fullSat);
w('sparkdown wire -> unified (parse in wasm)', fullWire);
w('remark (pure JS)', fullRemark);
console.log();
// The decisive nuance: the TRANSFORM itself is JS on both sides and costs ~the
// same. Sätteri's win is the tree never leaving Rust (no JS materialize / hast /
// stringify) — NOT a faster transform.
console.log(`  sätteri transform cost   (full - base):  ${(fullSat - fullSatBase).toFixed(2)} ms   (base, no plugin: ${fullSatBase.toFixed(2)} ms)`);
console.log(`  sparkdown transform cost (stage 2)    :  ${tTransform.toFixed(2)} ms`);
console.log('  → transform ≈ a tie (both run JS visitors); the gap is sparkdown doing');
console.log('    materialize + mdast→hast + hast→html in JS (stages 1,3,4) vs Sätteri in Rust.');
