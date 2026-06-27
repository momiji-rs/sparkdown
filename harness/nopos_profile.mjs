// Decompose toMdast (no-position) into wasm emit / wire-walk+string-decode /
// object-creation, to find what to attack to beat satteri's 1.37 ms lazy handle.
import { readFileSync } from 'node:fs';

const wasmBytes = readFileSync(new URL('./sparkdown.wasm', import.meta.url));
const { instance } = await WebAssembly.instantiate(wasmBytes, {});
const ex = instance.exports;
const enc = new TextEncoder();
const dec = new TextDecoder();
const REFTYPE = ['shortcut', 'collapsed', 'full'];
const md = readFileSync(new URL('../tests/fixtures/data.md', import.meta.url), 'utf8');
const inputBytes = enc.encode(md);

// mode: 0 = wasm only (no read), 1 = walk (read+decode strings, no objects),
//       2 = build WITH position:undefined key, 3 = build WITHOUT position key
function run(mode) {
  const inPtr = ex.sparkdown_alloc(inputBytes.length);
  new Uint8Array(ex.memory.buffer).set(inputBytes, inPtr);
  const ptr = ex.sparkdown_to_mdast_wire_nopos_opts(inPtr, inputBytes.length, 0);
  const mem = ex.memory.buffer;
  const dv = new DataView(mem);
  const total = dv.getUint32(ptr, true);
  let result = total;
  if (mode > 0) {
    const base = ptr + 4;
    const u8 = new Uint8Array(mem, base, total);
    let p = 0;
    const u32 = () => { const v = dv.getUint32(base + p, true); p += 4; return v; };
    const str = () => {
      const hdr = u32();
      const n = hdr & 0x7fffffff;
      const end = p + n;
      const s = hdr >>> 31 && n <= 512 ? String.fromCharCode.apply(null, u8.subarray(p, end)) : dec.decode(u8.subarray(p, end));
      p = end;
      return s;
    };
    const opt = () => { if (dv.getUint32(base + p, true) === 0xffffffff) { p += 4; return null; } return str(); };
    const kids = () => { const n = u32(); if (mode === 1) { for (let i = 0; i < n; i++) node(); return null; } const a = new Array(n); for (let i = 0; i < n; i++) a[i] = node(); return a; };
    const attrs = () => { const n = u32(); const o = {}; for (let i = 0; i < n; i++) { const k = str(); o[k] = str(); } return o; };
    function node() {
      const tag = u8[p++];
      const K = mode === 2 ? 1 : 0; // include position key?
      switch (tag) {
        case 0: { const c = kids(); return mode === 1 ? 0 : K ? { type: 'root', children: c, position: undefined } : { type: 'root', children: c }; }
        case 1: { const c = kids(); return mode === 1 ? 0 : K ? { type: 'paragraph', children: c, position: undefined } : { type: 'paragraph', children: c }; }
        case 2: { const depth = u8[p++]; const c = kids(); return mode === 1 ? 0 : K ? { type: 'heading', depth, children: c, position: undefined } : { type: 'heading', depth, children: c }; }
        case 3: { const c = kids(); return mode === 1 ? 0 : { type: 'blockquote', children: c }; }
        case 4: { const f = u8[p++]; const st = u32(); const c = kids(); return mode === 1 ? 0 : { type: 'list', ordered: !!(f & 1), start: st === 0xffffffff ? null : st, spread: !!(f & 2), children: c }; }
        case 5: { const spread = !!u8[p++]; const ck = u8[p++]; const c = kids(); return mode === 1 ? 0 : { type: 'listItem', spread, checked: ck === 2 ? null : ck === 1, children: c }; }
        case 6: return mode === 1 ? 0 : { type: 'thematicBreak' };
        case 7: { const lang = opt(); const meta = opt(); const value = str(); return mode === 1 ? 0 : { type: 'code', lang, meta, value }; }
        case 8: { const v = str(); return mode === 1 ? 0 : { type: 'html', value: v }; }
        case 9: { const v = str(); return mode === 1 ? 0 : K ? { type: 'text', value: v, position: undefined } : { type: 'text', value: v }; }
        case 10: { const c = kids(); return mode === 1 ? 0 : { type: 'emphasis', children: c }; }
        case 11: { const c = kids(); return mode === 1 ? 0 : { type: 'strong', children: c }; }
        case 12: { const c = kids(); return mode === 1 ? 0 : { type: 'delete', children: c }; }
        case 13: { const v = str(); return mode === 1 ? 0 : { type: 'inlineCode', value: v }; }
        case 14: return mode === 1 ? 0 : { type: 'break' };
        case 15: { const url = str(); const title = opt(); const c = kids(); return mode === 1 ? 0 : { type: 'link', url, title, children: c }; }
        case 16: { const url = str(); const title = opt(); const alt = str(); return mode === 1 ? 0 : { type: 'image', url, title, alt }; }
        case 17: { const identifier = str(); const label = str(); const url = str(); const title = opt(); return mode === 1 ? 0 : { type: 'definition', identifier, label, url, title }; }
        case 18: { const identifier = str(); const label = str(); const rt = REFTYPE[u8[p++]]; const c = kids(); return mode === 1 ? 0 : { type: 'linkReference', identifier, label, referenceType: rt, children: c }; }
        case 19: { const identifier = str(); const label = str(); const rt = REFTYPE[u8[p++]]; const alt = str(); return mode === 1 ? 0 : { type: 'imageReference', identifier, label, referenceType: rt, alt }; }
        case 31: { const ncols = u32(); p += ncols; const c = kids(); return mode === 1 ? 0 : { type: 'table', children: c }; }
        case 32: { const c = kids(); return mode === 1 ? 0 : { type: 'tableRow', children: c }; }
        case 33: { const c = kids(); return mode === 1 ? 0 : { type: 'tableCell', children: c }; }
        default: throw new Error('tag ' + tag);
      }
    }
    result = node();
  }
  ex.sparkdown_free(ptr, 4 + total);
  ex.sparkdown_free(inPtr, inputBytes.length);
  return result;
}

const bench = (fn) => { for (let i = 0; i < 30; i++) fn(); let b = Infinity; for (let t = 0; t < 25; t++) { const t0 = performance.now(); for (let i = 0; i < 30; i++) fn(); const ms = (performance.now() - t0) / 30; if (ms < b) b = ms; } return b; };

const wasmOnly = bench(() => run(0));
const walk = bench(() => run(1));
const buildKey = bench(() => run(2));
const buildNoKey = bench(() => run(3));
console.log('\nno-position toMdast decomposition (200 KB spec, best-of-25):\n');
console.log(`  wasm emit only (no JS read)        ${wasmOnly.toFixed(3)} ms`);
console.log(`  + walk wire + decode strings       ${walk.toFixed(3)} ms   (read/decode: +${(walk - wasmOnly).toFixed(3)})`);
console.log(`  + build objects (position: undef)  ${buildKey.toFixed(3)} ms   (objects: +${(buildKey - walk).toFixed(3)})`);
console.log(`  + build objects (no position key)  ${buildNoKey.toFixed(3)} ms   (key cost: ${(buildKey - buildNoKey).toFixed(3)})`);
