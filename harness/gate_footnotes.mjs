// Alignment gate (task #9): sparkdown's native GFM footnotes vs remark-gfm.
//   Gate 1 — mdast structural (ignore position)
//   Gate 2 — mdast with position (byte-for-byte)
//   Gate 3 — HTML vs remark-rehype (the chosen HTML target; native == fallback)
//
// The HTML corpus uses simple prose so the non-footnote HTML matches on both
// sides (sparkdown's general to_html is cmark-shaped; the footnote machinery is
// built to the remark-rehype shape). Run: node gate_footnotes.mjs

import { unified } from 'unified';
import remarkParse from 'remark-parse';
import remarkGfm from 'remark-gfm';
import remarkRehype from 'remark-rehype';
import rehypeStringify from 'rehype-stringify';
import { runGate, eqMdast, eqJson } from './gate_harness.mjs';
import { parseToMdastWire } from './sparkdown.mjs';
import { sparkOpts } from './perf_harness.mjs';

const FOOTNOTES = 512; // flag bit 9

const CORPUS = [
  'A[^a].\n\n[^a]: note\n',
  'A[^a] and again[^a]. Also[^b].\n\n[^a]: first\n[^b]: second\n',
  'ref without def [^x] stays literal\n',
  'A[^Foo]\n\n[^foo]: case-insensitive\n',
  'A[^a]\n\n[^a]: one\n\n    two\n', // multi-paragraph def
  'A[^a]\n\n[^a]: one\nlazy line\n', // lazy continuation
  'A[^a]\n\n[^a]:\n    - item\n', // non-paragraph tail
  'A[^1] B[^2]\n\n[^1]: one\n[^2]: two\n', // numeric labels, order
  'unused defs dropped\n\n[^a]: never referenced\n',
  'A[^a]\n\n[^a]: first\n\n[^a]: second wins? no, first\n', // duplicate defs
  'text before\n[^a]: interrupts paragraph\n\nuse [^a]\n',
  'A[^a]\n\n[^a]: with *emphasis* and `code`\n',
  'three refs [^a] [^a] [^a]\n\n[^a]: backrefs\n',
  'A[^a]\n\n> [^a]: in a blockquote\n', // def inside blockquote (edge)
];

const refTree = unified().use(remarkParse).use(remarkGfm);
const refHtml = unified().use(remarkParse).use(remarkGfm).use(remarkRehype).use(rehypeStringify);

console.log('footnotes alignment gate — sparkdown (native) vs remark-gfm / remark-rehype\n');
const g1 = runGate({
  label: 'Gate 1 — mdast structural (ignore position)',
  items: CORPUS,
  a: (md) => parseToMdastWire(md, FOOTNOTES),
  b: (md) => refTree.parse(md),
  eq: eqMdast({ dropPos: true }),
});
const g2 = runGate({
  label: 'Gate 2 — mdast with position',
  items: CORPUS,
  a: (md) => parseToMdastWire(md, FOOTNOTES),
  b: (md) => refTree.parse(md),
  eq: eqMdast({ dropPos: false }),
});
// HTML gate: only cases that emit a footnote <section>. Cases with no resolved
// reference reduce to a plain paragraph, where sparkdown's cmark-shaped to_html
// and remark-rehype differ on a trailing newline (a pre-existing, non-footnote
// divergence; the mdast for those is gated above and matches exactly).
const HTML_CORPUS = CORPUS.filter((md) => sparkOpts(md, FOOTNOTES).includes('data-footnotes'));
const g3 = runGate({
  label: `Gate 3 — HTML vs remark-rehype (${HTML_CORPUS.length} section-producing)`,
  items: HTML_CORPUS,
  a: (md) => sparkOpts(md, FOOTNOTES),
  b: (md) => String(refHtml.processSync(md)),
  eq: eqJson,
});

process.exit(g1.ok && g2.ok && g3.ok ? 0 : 1);
