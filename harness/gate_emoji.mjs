// Alignment gate (task #13): sparkdown's native emoji vs remark-gemoji.
// Simple prose corpus, so the non-emoji HTML matches on both sides (cmark and
// rehype agree on plain paragraphs). Run: node gate_emoji.mjs

import { unified } from 'unified';
import remarkParse from 'remark-parse';
import remarkGemoji from 'remark-gemoji';
import remarkRehype from 'remark-rehype';
import rehypeStringify from 'rehype-stringify';
import { nameToEmoji } from 'gemoji';
import { runGate, eqJson } from './gate_harness.mjs';
import { sparkOpts } from './perf_harness.mjs';

const EMOJI = 1024;

// Hand-picked edge cases + a broad sweep over the whole gemoji dataset.
const EDGE = [
  ':smile:\n',
  'a:smile:b\n',
  ':notanemoji:\n',
  '::smile::\n',
  ':+1: and :-1:\n',
  'in `code :smile:` stays\n',
  ':smile\n',
  'text :smile: more :heart: end\n',
  ':SMILE: (uppercase, literal)\n',
  'no:space:here\n',
  'emoji in *:smile:* emphasis\n',
  'link [text](:smile:) target\n',
];
// Sweep: every shortcode in a sentence (catches any table/encoding mismatch).
const ALL = Object.keys(nameToEmoji).map((k) => `say :${k}: now\n`);
const CORPUS = [...EDGE, ...ALL];

const ref = unified().use(remarkParse).use(remarkGemoji).use(remarkRehype).use(rehypeStringify);

console.log(`emoji alignment gate — sparkdown (native) vs remark-gemoji (${CORPUS.length} cases)\n`);
// Compare trailing-trimmed: sparkdown's cmark-shaped to_html ends a paragraph
// with `\n`, remark-rehype does not — a pre-existing, non-emoji divergence. The
// emoji substitution itself is what this gate verifies.
const g = runGate({
  label: 'HTML vs remark-gemoji (trailing-trimmed)',
  items: CORPUS,
  a: (md) => sparkOpts(md, EMOJI).trimEnd(),
  b: (md) => String(ref.processSync(md)).trimEnd(),
  eq: eqJson,
  limit: 12,
});

process.exit(g.ok ? 0 : 1);
