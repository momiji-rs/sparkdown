// SPIKE: the wasm→JS boundary.
//
// The premise of a "Rust core + JS plugins" product (à la Sätteri) is that
// parsing in Rust/wasm and shipping the tree to JS beats parsing in JS. This
// measures whether that holds for the realistic JSON-boundary design:
//
//   wasm path:  write src → sparkdown_to_mdast_json (parse+serialize in wasm)
//               → read bytes out → TextDecoder → JSON.parse → JS mdast tree
//   JS path:    mdast-util-from-markdown(src) → JS mdast tree   (what remark does)
//
// Both end with an equivalent JS mdast tree in hand (the thing a plugin walks).
// We also break the wasm path into call / decode / JSON.parse to locate the cost.

import { readFileSync } from 'node:fs';
import { fromMarkdown } from 'mdast-util-from-markdown';
import { visit } from 'unist-util-visit';

const DATA = readFileSync(new URL('../tests/fixtures/data.md', import.meta.url));
const wasmBytes = readFileSync(new URL('./sparkdown.wasm', import.meta.url));
const { instance } = await WebAssembly.instantiate(wasmBytes, {});
const x = instance.exports;
const mem = () => new Uint8Array(x.memory.buffer);

const enc = new TextEncoder();
const dec = new TextDecoder();
const srcStr = DATA.toString('utf8'); // decoded once; fair to the JS path
const srcBytes = enc.encode(srcStr);

// Pre-allocate the input region once; in real use you'd write each new doc here.
const inPtr = x.sparkdown_alloc(srcBytes.length);

function writeInput() {
  mem().set(srcBytes, inPtr);
}
function readResult(ptr) {
  const dv = new DataView(x.memory.buffer);
  const len = dv.getUint32(ptr, true);
  const bytes = new Uint8Array(x.memory.buffer, ptr + 4, len);
  return { len, bytes };
}

// --- one full wasm-path run → JS tree ---
function wasmToTree() {
  writeInput();
  const ptr = x.sparkdown_to_mdast_json(inPtr, srcBytes.length);
  const { len, bytes } = readResult(ptr);
  const json = dec.decode(bytes);
  const tree = JSON.parse(json);
  x.sparkdown_free(ptr, 4 + len);
  return tree;
}

// --- timing helper ---
function time(iters, fn) {
  const warm = Math.max(20, iters / 5) | 0;
  for (let i = 0; i < warm; i++) fn();
  const t = performance.now();
  for (let i = 0; i < iters; i++) fn();
  return ((performance.now() - t) / iters) * 1000; // µs/op
}

// --- sanity: the wasm tree is real, traversable, comparable size ---
const tree = wasmToTree();
let nodes = 0;
visit(tree, () => nodes++);
const refTree = fromMarkdown(srcStr);
let refNodes = 0;
visit(refTree, () => refNodes++);
const jsonBytes = (() => {
  writeInput();
  const ptr = x.sparkdown_to_mdast_json(inPtr, srcBytes.length);
  const { len } = readResult(ptr);
  x.sparkdown_free(ptr, 4 + len);
  return len;
})();

const iters = 200;

// JS baseline: parse markdown to an mdast tree in pure JS.
const tJs = time(iters, () => fromMarkdown(srcStr));

// wasm path, end to end.
const tWasmTotal = time(iters, () => wasmToTree());

// Breakdown.
const tCall = time(iters, () => {
  writeInput();
  const ptr = x.sparkdown_to_mdast_json(inPtr, srcBytes.length);
  const { len } = readResult(ptr); // read length only (parse+serialize in wasm)
  x.sparkdown_free(ptr, 4 + len);
});
const tDecode = time(iters, () => {
  writeInput();
  const ptr = x.sparkdown_to_mdast_json(inPtr, srcBytes.length);
  const { len, bytes } = readResult(ptr);
  dec.decode(bytes); // + UTF-8 decode to JS string
  x.sparkdown_free(ptr, 4 + len);
});
const tParseOnly = (() => {
  // isolate JSON.parse: prebuild the json string once, parse repeatedly.
  writeInput();
  const ptr = x.sparkdown_to_mdast_json(inPtr, srcBytes.length);
  const { len, bytes } = readResult(ptr);
  const json = dec.decode(bytes);
  x.sparkdown_free(ptr, 4 + len);
  return time(iters, () => JSON.parse(json));
})();

// Reference: pure parse across the boundary (to_html, no mdast/json) for context.
const tHtml = time(iters, () => {
  writeInput();
  const ptr = x.sparkdown_to_html(inPtr, srcBytes.length);
  const { len } = readResult(ptr);
  x.sparkdown_free(ptr, 4 + len);
});

x.sparkdown_free(inPtr, srcBytes.length);

const KB = (DATA.length / 1024).toFixed(0);
const us = (n) => n.toFixed(1).padStart(8);
console.log(`\nwasm→JS boundary — CommonMark spec doc (${KB} KB), ${iters} iters\n`);
console.log(`  wasm mdast nodes: ${nodes}   (remark from-markdown: ${refNodes})`);
console.log(`  mdast JSON size : ${(jsonBytes / 1024).toFixed(0)} KB\n`);
console.log(`  ${'path'.padEnd(40)} ${'µs/op'.padStart(8)}  ${'vs JS'.padStart(7)}`);
console.log(`  ${'-'.repeat(40)} ${'-'.repeat(8)}  ${'-'.repeat(7)}`);
const row = (name, t) => console.log(`  ${name.padEnd(40)} ${us(t)}  ${(t / tJs).toFixed(2).padStart(6)}x`);
row('JS: mdast-util-from-markdown → tree', tJs);
row('wasm: → JS mdast tree (TOTAL)', tWasmTotal);
console.log('  ' + '·'.repeat(40));
row('  wasm call (parse+serialize+boundary)', tCall);
row('  + TextDecoder (bytes→JS string)', tDecode);
row('  JSON.parse (string→JS objects)', tParseOnly);
row('  [ref] wasm to_html (parse only)', tHtml);
console.log();
console.log(`  verdict: wasm path is ${(tJs / tWasmTotal).toFixed(2)}× ${tWasmTotal < tJs ? 'FASTER' : 'SLOWER'} than parsing in JS.`);
console.log();
