// Perf verification (task #13): native emoji vs the remark-gemoji JS pipeline
// (the ecosystem way to get emoji — Sätteri has no emoji feature). Run:
//   node perf_emoji.mjs

import { unified } from 'unified';
import remarkParse from 'remark-parse';
import remarkGemoji from 'remark-gemoji';
import remarkRehype from 'remark-rehype';
import rehypeStringify from 'rehype-stringify';
import { best, report, sparkOpts } from './perf_harness.mjs';

const EMOJI = 1024;

// An emoji-dense document: 200 lines each with a couple of shortcodes.
const codes = ['smile', 'heart', 'rocket', '+1', 'tada', 'fire', 'eyes', 'wave'];
let doc = '';
for (let i = 0; i < 200; i++) {
  const a = codes[i % codes.length];
  const b = codes[(i + 3) % codes.length];
  doc += `Line ${i} is feeling :${a}: today and a bit :${b}: too.\n\n`;
}

const ref = unified().use(remarkParse).use(remarkGemoji).use(remarkRehype).use(rehypeStringify);

report('emoji-dense doc → HTML (200 lines, 2 codes each), best-of-15', [
  { name: 'sparkdown built-in (all wasm)', ms: best(() => sparkOpts(doc, EMOJI), { iters: 50 }) },
  { name: 'remark-gemoji pipeline (full JS)', ms: best(() => String(ref.processSync(doc)), { iters: 20 }) },
]);
