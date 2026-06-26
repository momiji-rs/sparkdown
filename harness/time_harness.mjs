// Deterministic per-parse cost via `/usr/bin/time -l` instructions-retired.
// Loads BOTH engines (constant startup), then runs the chosen one N times; the
// wide-gap (N=12000 minus N=2000)/10000 cancels startup. argv: which N
import { readFileSync } from 'node:fs';
import { markdownToMdast } from 'satteri';
import { toMdastSync, initSync } from '../npm/mdast.mjs';

const ex = initSync();
const md = readFileSync(new URL('../tests/fixtures/data.md', import.meta.url), 'utf8');
const which = process.argv[2];
const N = +process.argv[3];
const satFeat = { features: { gfm: false, frontmatter: false } };
const inputBytes = new TextEncoder().encode(md);

// wasm half only: parse + emit wire + free, no JS tree build
function wasmOnly() {
  const inPtr = ex.sparkdown_alloc(inputBytes.length);
  new Uint8Array(ex.memory.buffer).set(inputBytes, inPtr);
  const ptr = ex.sparkdown_to_mdast_wire_nopos_opts(inPtr, inputBytes.length, 0);
  const total = new DataView(ex.memory.buffer).getUint32(ptr, true);
  ex.sparkdown_free(ptr, 4 + total);
  ex.sparkdown_free(inPtr, inputBytes.length);
  return total;
}

// direct to_html: parse + render (no wire serialization, no JS tree) — the parse floor
function htmlOnly() {
  const inPtr = ex.sparkdown_alloc(inputBytes.length);
  new Uint8Array(ex.memory.buffer).set(inputBytes, inPtr);
  const ptr = ex.sparkdown_to_html(inPtr, inputBytes.length);
  const total = new DataView(ex.memory.buffer).getUint32(ptr, true);
  ex.sparkdown_free(ptr, 4 + total);
  ex.sparkdown_free(inPtr, inputBytes.length);
  return total;
}

const fn =
  which === 'satteri' ? () => markdownToMdast(md, satFeat)
  : which === 'pos' ? () => toMdastSync(md)
  : which === 'wasmonly' ? wasmOnly
  : which === 'html' ? htmlOnly
  : () => toMdastSync(md, { position: false }); // 'nopos'

let sink = 0;
for (let i = 0; i < N; i++) sink ^= fn() ? 1 : 0;
console.error('sink=' + sink);
