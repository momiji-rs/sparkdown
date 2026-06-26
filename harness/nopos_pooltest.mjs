// Validate the string-pool hypothesis BEFORE building the wire format: is
// "decode the whole pool once + substring per string" faster than "TextDecoder
// per string"? Extract the real string ranges from the no-position wire.
import { readFileSync } from 'node:fs';

const wasmBytes = readFileSync(new URL('./sparkdown.wasm', import.meta.url));
const { instance } = await WebAssembly.instantiate(wasmBytes, {});
const ex = instance.exports;
const enc = new TextEncoder();
const dec = new TextDecoder();
const md = readFileSync(new URL('../tests/fixtures/data.md', import.meta.url), 'utf8');
const inputBytes = enc.encode(md);

// Parse the wire once, collecting every string's [offset,len] (offset into u8).
const inPtr = ex.sparkdown_alloc(inputBytes.length);
new Uint8Array(ex.memory.buffer).set(inputBytes, inPtr);
const ptr = ex.sparkdown_to_mdast_wire_nopos_opts(inPtr, inputBytes.length, 0);
const mem = ex.memory.buffer;
const dv = new DataView(mem);
const total = dv.getUint32(ptr, true);
const base = ptr + 4;
const u8 = new Uint8Array(mem.slice(base, base + total)); // stable copy for the test
let p = 0;
const ranges = [];
const u32 = () => { const v = (u8[p] | (u8[p + 1] << 8) | (u8[p + 2] << 16) | (u8[p + 3] << 24)) >>> 0; p += 4; return v; };
const strR = () => { const n = u32() & 0x7fffffff; ranges.push([p, n]); p += n; };
const optR = () => { if (((u8[p] | (u8[p + 1] << 8) | (u8[p + 2] << 16) | (u8[p + 3] << 24)) >>> 0) === 0xffffffff) { p += 4; return; } strR(); };
const kids = () => { const n = u32(); for (let i = 0; i < n; i++) node(); };
function node() {
  const tag = u8[p++];
  switch (tag) {
    case 0: case 1: case 3: case 10: case 11: case 12: return kids();
    case 2: p++; return kids();
    case 4: { p++; u32(); return kids(); }
    case 5: { p++; return kids(); }
    case 6: case 14: return;
    case 7: { optR(); optR(); strR(); return; }
    case 8: case 9: case 13: return strR();
    case 15: { strR(); optR(); return kids(); }
    case 16: { strR(); optR(); strR(); return; }
    case 17: { strR(); strR(); strR(); optR(); return; }
    case 18: { strR(); strR(); p++; return kids(); }
    case 19: { strR(); strR(); p++; strR(); return; }
    default: throw new Error('tag ' + tag);
  }
}
node();
ex.sparkdown_free(ptr, 4 + total);
ex.sparkdown_free(inPtr, inputBytes.length);

// Build the pool: all string bytes contiguous + char offsets (ASCII assumed for
// this perf probe — most strings are; the real build computes UTF-16 offsets).
let poolLen = 0;
for (const [, n] of ranges) poolLen += n;
const pool = new Uint8Array(poolLen);
const charOff = new Array(ranges.length);
let off = 0;
for (let i = 0; i < ranges.length; i++) { const [o, n] = ranges[i]; pool.set(u8.subarray(o, o + n), off); charOff[i] = off; off += n; }

console.log(`${ranges.length} strings, ${(poolLen / 1024).toFixed(0)} KB of text\n`);

function perString() { const out = new Array(ranges.length); for (let i = 0; i < ranges.length; i++) { const [o, n] = ranges[i]; out[i] = n === 0 ? '' : dec.decode(u8.subarray(o, o + n)); } return out; }
function poolDecode() { const S = dec.decode(pool); const out = new Array(ranges.length); for (let i = 0; i < ranges.length; i++) { const o = charOff[i]; out[i] = S.substring(o, o + ranges[i][1]); } return out; }

// correctness (ASCII subset)
const a = perString(), b = poolDecode();
let same = true; for (let i = 0; i < a.length; i++) if (a[i] !== b[i]) { same = false; break; }
console.log('pool == per-string (ascii corpus):', same);

const bench = (fn) => { for (let i = 0; i < 40; i++) fn(); let bb = Infinity; for (let t = 0; t < 30; t++) { const t0 = performance.now(); for (let i = 0; i < 40; i++) fn(); const ms = (performance.now() - t0) / 40; if (ms < bb) bb = ms; } return bb; };
for (let trial = 1; trial <= 3; trial++) {
  console.log(`trial ${trial}:  per-string ${bench(perString).toFixed(3)} ms   pool-decode ${bench(poolDecode).toFixed(3)} ms`);
}
