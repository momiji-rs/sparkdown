// SPIKE: the real consumer-facing usage. sparkdown plugged into unified exactly
// like remark-parse — `unified().use(sparkdown).use(...plugins).process(md)` —
// and shown to produce the same output as the genuine remark-parse pipeline.

import { unified } from 'unified';
import remarkParse from 'remark-parse';
import remarkToc from 'remark-toc';
import remarkEmoji from 'remark-emoji';
import remarkRehype from 'remark-rehype';
import rehypeSlug from 'rehype-slug';
import rehypeStringify from 'rehype-stringify';
import { readFileSync } from 'node:fs';

import sparkdown from './sparkdown.mjs';

// Identical plugin stacks; only the parser differs.
const plugins = (p) =>
  p
    .use(remarkEmoji)
    .use(remarkToc, { heading: 'contents', tight: true })
    .use(remarkRehype, { allowDangerousHtml: true })
    .use(rehypeSlug)
    .use(rehypeStringify, { allowDangerousHtml: true });

const sparkdownProcessor = plugins(unified().use(sparkdown));
const remarkProcessor = plugins(unified().use(remarkParse));

const md = `# sparkdown × unified

## Contents

## Getting started

Install once, then \`unified().use(sparkdown)\` :tada:. It replaces
[remark-parse](https://github.com/remarkjs/remark) — parsing happens in wasm.

## Notes

- works with *any* mdast/hast transform plugin
- ~13–23× faster parse than micromark
`;

const ours = String(await sparkdownProcessor.process(md));
const ref = String(await remarkProcessor.process(md));

console.log('\n=== consumer usage: unified().use(sparkdown).use(...plugins).process(md) ===\n');
console.log(ours);
console.log('--- vs the same pipeline on remark-parse ---');
console.log('  identical output:', ours === ref ? 'YES ✅' : 'NO ❌');

// Generalize across the corpus: same plugin stack, sparkdown parser vs remark-parse.
const corpus = JSON.parse(readFileSync(new URL('./sparkdown-mdast.json', import.meta.url), 'utf8'));
let same = 0;
const diffs = [];
for (const e of corpus) {
  let a, b;
  try { a = String(await sparkdownProcessor.process(e.markdown)); } catch { a = 'ERR_A'; }
  try { b = String(await remarkProcessor.process(e.markdown)); } catch { b = 'ERR_B'; }
  if (a === b) same++; else diffs.push(e.example);
}
console.log('\n--- full pipeline parity across 652 CommonMark examples ---');
console.log(`  unified(sparkdown) vs unified(remark-parse), same plugins: ${same} / ${corpus.length} (${((100 * same) / corpus.length).toFixed(1)}%) identical HTML`);
if (diffs.length) console.log(`  differ (${diffs.length}): ${diffs.slice(0, 20).join(', ')}${diffs.length > 20 ? ' …' : ''}`);
console.log();
