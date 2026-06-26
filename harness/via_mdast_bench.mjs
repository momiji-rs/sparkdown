// Perf spike: where does the time go, and can the Rust mdast→html render get the
// "via mdast" path under satteri? Warm best-of-N, 200 KB CommonMark spec.
import { readFileSync } from 'node:fs';
import { markdownToHtml } from 'satteri';
import { unified } from 'unified';
import remarkParse from 'remark-parse';
import remarkRehype from 'remark-rehype';
import rehypeStringify from 'rehype-stringify';

const wasmBytes = readFileSync(new URL('./sparkdown.wasm', import.meta.url));
const { instance } = await WebAssembly.instantiate(wasmBytes, {});
const ex = instance.exports;
const enc = new TextEncoder();
const dec = new TextDecoder();

function callWasm(fn, md, flags) {
  const input = enc.encode(md);
  const p = ex.sparkdown_alloc(input.length);
  new Uint8Array(ex.memory.buffer).set(input, p);
  const out = flags === undefined ? ex[fn](p, input.length) : ex[fn](p, input.length, flags);
  const buf = ex.memory.buffer;
  const len = new DataView(buf).getUint32(out, true);
  const s = dec.decode(new Uint8Array(buf, out + 4, len));
  ex.sparkdown_free(p, input.length);
  ex.sparkdown_free(out, 4 + len);
  return s;
}

const md = readFileSync(new URL('../tests/fixtures/data.md', import.meta.url), 'utf8');
const remarkProc = unified().use(remarkParse).use(remarkRehype).use(rehypeStringify);
const satteriFeat = { features: { gfm: false, frontmatter: false } };

function bench(fn) {
  for (let i = 0; i < 20; i++) fn();
  let best = Infinity;
  for (let t = 0; t < 15; t++) {
    const t0 = performance.now();
    for (let i = 0; i < 20; i++) fn();
    const ms = (performance.now() - t0) / 20;
    if (ms < best) best = ms;
  }
  return best;
}

const rows = [
  ['sparkdown to_html (wasm, direct)', () => callWasm('sparkdown_to_html', md)],
  ['sparkdown via mdast→html (wasm, Rust render)', () => callWasm('sparkdown_to_html_via_mdast_opts', md, 0)],
  ['satteri (Rust napi) markdownToHtml', () => markdownToHtml(md, satteriFeat)],
  ['remark (pure JS) full unified pipeline', () => remarkProc.processSync(md).toString()],
];

console.log(`\nmd → HTML, 200 KB CommonMark spec, best-of-15 (warm):\n`);
const base = bench(rows[1][1]); // via-mdast as the reference
for (const [name, fn] of rows) {
  const ms = bench(fn);
  console.log(`  ${name.padEnd(46)} ${ms.toFixed(3)} ms   ${(ms / base).toFixed(2)}×`);
}
