// End-to-end verify + benchmark of the published @momiji-rs/sparkdown/mdast entry:
// (1) toMdast deep-equals mdast-util-from-markdown, (2) the sparkdownParse unified
// pipeline produces the same HTML as the remark pipeline, (3) md→mdast benchmark.
import { readFileSync } from 'node:fs';
import { fromMarkdown } from 'mdast-util-from-markdown';
import { unified } from 'unified';
import remarkParse from 'remark-parse';
import remarkRehype from 'remark-rehype';
import rehypeStringify from 'rehype-stringify';
import { markdownToMdast } from 'satteri';
import sparkdownParse, { toMdastSync, toHtmlSync, initSync } from '../npm/mdast.mjs';

initSync();
const data = JSON.parse(readFileSync(new URL('./sparkdown-mdast.json', import.meta.url), 'utf8'));

function canon(n, dropPos) {
  if (Array.isArray(n)) return n.map((x) => canon(x, dropPos));
  if (n && typeof n === 'object') {
    const o = {};
    for (const k of Object.keys(n).sort()) {
      if (k === 'position' && dropPos) continue;
      if (n[k] === undefined) continue;
      o[k] = canon(n[k], dropPos);
    }
    return o;
  }
  return n;
}
const eq = (a, b, dropPos) => JSON.stringify(canon(a, dropPos)) === JSON.stringify(canon(b, dropPos));

// (1) drop-in remark-parse: toMdast deep-equals the canonical JS parser
let g1 = 0, g2 = 0;
for (const e of data) {
  const s = toMdastSync(e.markdown), r = fromMarkdown(e.markdown);
  if (eq(s, r, true)) g1++;
  if (eq(s, r, false)) g2++;
}
console.log(`toMdast == mdast-util-from-markdown : ${g1}/${data.length} ignoring position, ${g2}/${data.length} including position`);

// (2) drop-in in a real unified pipeline: same HTML as remark-parse
const sparkProc = unified().use(sparkdownParse).use(remarkRehype).use(rehypeStringify);
const remarkProc = unified().use(remarkParse).use(remarkRehype).use(rehypeStringify);
let uni = 0;
for (const e of data) {
  if (String(sparkProc.processSync(e.markdown)) === String(remarkProc.processSync(e.markdown))) uni++;
}
console.log(`sparkdownParse→unified HTML == remark→unified HTML : ${uni}/${data.length}`);

// (3) toHtml (the in-wasm mdast→html render) sanity
const sample = '# Hi *there*\n\n- a\n- b\n';
console.log(`toHtmlSync sample: ${JSON.stringify(toHtmlSync(sample))}`);

// (4) benchmark — md → mdast (the parse we replace)
const md = readFileSync(new URL('../tests/fixtures/data.md', import.meta.url), 'utf8');
const satFeat = { features: { gfm: false, frontmatter: false } };
const bench = (fn) => {
  for (let i = 0; i < 20; i++) fn();
  let b = Infinity;
  for (let t = 0; t < 15; t++) {
    const t0 = performance.now();
    for (let i = 0; i < 20; i++) fn();
    const ms = (performance.now() - t0) / 20;
    if (ms < b) b = ms;
  }
  return b;
};
const rows = [
  ['sparkdown toMdast (wasm wire, materialized)', () => toMdastSync(md)],
  ['remark-parse (mdast-util-from-markdown)', () => fromMarkdown(md)],
  ['satteri markdownToMdast (lazy handle)', () => markdownToMdast(md, satFeat)],
];
console.log('\nmd → mdast, 200 KB CommonMark spec, best-of-15 (warm):');
const base = bench(rows[0][1]);
for (const [n, fn] of rows) {
  const ms = bench(fn);
  console.log(`  ${n.padEnd(44)} ${ms.toFixed(3)} ms   ${(ms / base).toFixed(2)}×`);
}
