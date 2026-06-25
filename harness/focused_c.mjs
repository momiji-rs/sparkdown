// Focused, high-trial micro-bench for the headline claim: produce a FULLY
// materialized mdast tree in JS (every node reachable). best-of-15.
// Run native:  node focused_c.mjs
// Run satteri wasi: NAPI_RS_FORCE_WASI=1 node focused_c.mjs
import { readFileSync } from 'node:fs';
import { markdownToMdast } from 'satteri';
import { parseToMdast, parseToMdastWire } from './sparkdown.mjs';

const MD = readFileSync(new URL('../tests/fixtures/data.md', import.meta.url), 'utf8');
const feat = { features: { gfm: false } };
const walk = (t) => { let n = 1; for (const c of t.children || []) n += walk(c); return n; };
const sat = process.env.NAPI_RS_FORCE_WASI ? 'satteri (wasi)  markdownToMdast' : 'satteri (napi)  markdownToMdast';

function best(fn, iters = 200, trials = 15) {
  for (let i = 0; i < 50; i++) fn();
  let b = Infinity;
  for (let t = 0; t < trials; t++) {
    const s = performance.now();
    for (let i = 0; i < iters; i++) fn();
    b = Math.min(b, (performance.now() - s) / iters);
  }
  return b;
}

const cases = [
  [sat, () => walk(markdownToMdast(MD, feat))],
  ['sparkdown JSON -> JS  full', () => walk(parseToMdast(MD))],
  ['sparkdown WIRE -> JS  full ★', () => walk(parseToMdastWire(MD))],
];
// sanity: equal node counts
const ns = cases.map(([, f]) => f());
const rows = cases.map(([name, f], i) => ({ name, ms: best(f), n: ns[i] }));
const fastest = Math.min(...rows.map((r) => r.ms));
console.log(`\nFULLY materialized mdast — CommonMark spec (${(MD.length / 1024).toFixed(0)} KB), best-of-15\n`);
console.log(`  ${'combination'.padEnd(34)} ${'ms/op'.padStart(8)} ${'ops/s'.padStart(7)} ${'nodes'.padStart(6)} ${'vs best'.padStart(9)}`);
for (const r of rows)
  console.log(`  ${r.name.padEnd(34)} ${r.ms.toFixed(3).padStart(8)} ${(1000 / r.ms).toFixed(0).padStart(7)} ${String(r.n).padStart(6)} ${(r.ms / fastest).toFixed(2).padStart(8)}x`);
console.log();
