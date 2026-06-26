// Perf verification (task #9): native footnotes vs Sätteri vs the plain-object
// fallback (wire → mdast-util-to-hast → hast-util-to-html, i.e. the remark-rehype
// pipeline the compatible path uses).
//
// Run: node perf_footnotes.mjs

import { markdownToHtml } from 'satteri';
import { toHast } from 'mdast-util-to-hast';
import { toHtml } from 'hast-util-to-html';
import { best, report, sparkOpts } from './perf_harness.mjs';
import { parseToMdastWire } from './sparkdown.mjs';

const FOOTNOTES = 512;

// A footnote-heavy document: 60 paragraphs, each citing two footnotes, then 60
// definitions — a realistic heavy case for the collection + backref machinery.
const N = 60;
let body = '';
for (let i = 0; i < N; i++) {
  body += `Paragraph ${i} makes a point[^n${i}] and a second one[^m${i}] worth noting.\n\n`;
}
for (let i = 0; i < N; i++) {
  body += `[^n${i}]: First note number ${i} with some explanatory text.\n`;
  body += `[^m${i}]: Second note ${i}, also with a little detail.\n`;
}
const DOC = body;
const SAT = { features: { gfm: false, footnotes: true } };

// Fallback = the compatible path: sparkdown wire mdast → hast → HTML (what a user
// chaining remark-rehype gets).
const fallback = (md) =>
  toHtml(toHast(parseToMdastWire(md, FOOTNOTES), { allowDangerousHtml: true }), {
    allowDangerousHtml: true,
  });

report(`footnote-heavy doc → HTML (${N} footnotes ×2 refs), best-of-15`, [
  { name: 'sparkdown built-in (all wasm)', ms: best(() => sparkOpts(DOC, FOOTNOTES), { iters: 50 }) },
  { name: 'satteri (footnotes on)', ms: best(() => markdownToHtml(DOC, SAT).html, { iters: 50 }) },
  { name: 'sparkdown fallback (wire+JS+rehype)', ms: best(() => fallback(DOC), { iters: 30 }) },
]);
