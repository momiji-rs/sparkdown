// SPIKE: does the wasm-boundary win hold across doc sizes? (large doc tested in
// wasm_boundary.mjs; here we sweep small→large, since per-call fixed overhead
// matters most for small docs.)

import { readFileSync } from 'node:fs';
import { fromMarkdown } from 'mdast-util-from-markdown';

const DATA = readFileSync(new URL('../tests/fixtures/data.md', import.meta.url)).toString('utf8');
const wasmBytes = readFileSync(new URL('./sparkdown.wasm', import.meta.url));
const { instance } = await WebAssembly.instantiate(wasmBytes, {});
const x = instance.exports;
const enc = new TextEncoder();
const dec = new TextDecoder();

function wasmToTree(srcBytes) {
  const inPtr = x.sparkdown_alloc(srcBytes.length);
  new Uint8Array(x.memory.buffer).set(srcBytes, inPtr);
  const ptr = x.sparkdown_to_mdast_json(inPtr, srcBytes.length);
  const dv = new DataView(x.memory.buffer);
  const len = dv.getUint32(ptr, true);
  const json = dec.decode(new Uint8Array(x.memory.buffer, ptr + 4, len));
  const tree = JSON.parse(json);
  x.sparkdown_free(ptr, 4 + len);
  x.sparkdown_free(inPtr, srcBytes.length);
  return tree;
}
function time(iters, fn) {
  const warm = Math.max(50, iters / 5) | 0;
  for (let i = 0; i < warm; i++) fn();
  const t = performance.now();
  for (let i = 0; i < iters; i++) fn();
  return ((performance.now() - t) / iters) * 1000; // µs/op
}

// Build docs of a few sizes by taking newline-bounded prefixes of the spec.
function prefixBytes(targetKB) {
  const target = targetKB * 1024;
  let end = Math.min(target, DATA.length);
  while (end < DATA.length && DATA[end] !== '\n') end++;
  return DATA.slice(0, end);
}

console.log(`\nwasm→JS boundary vs pure-JS parse, by doc size\n`);
console.log(`  ${'doc'.padEnd(10)} ${'JS µs'.padStart(10)} ${'wasm µs'.padStart(10)} ${'speedup'.padStart(9)}`);
console.log(`  ${'-'.repeat(10)} ${'-'.repeat(10)} ${'-'.repeat(10)} ${'-'.repeat(9)}`);
for (const kb of [1, 5, 20, 100, 200]) {
  const str = prefixBytes(kb);
  const bytes = enc.encode(str);
  const iters = kb <= 5 ? 2000 : kb <= 20 ? 800 : 200;
  const tJs = time(iters, () => fromMarkdown(str));
  const tWasm = time(iters, () => wasmToTree(bytes));
  const label = `${(bytes.length / 1024).toFixed(0)}KB`;
  console.log(
    `  ${label.padEnd(10)} ${tJs.toFixed(1).padStart(10)} ${tWasm.toFixed(1).padStart(10)} ${(tJs / tWasm).toFixed(1).padStart(8)}x`,
  );
}
console.log();
