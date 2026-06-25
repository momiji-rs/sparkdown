// Profile where parseToMdastWire's time goes, to target the next optimization.
// Reimplements the reader with toggles so we can isolate each cost component.
import { readFileSync } from 'node:fs';

const wasmBytes = readFileSync(new URL('./sparkdown.wasm', import.meta.url));
const x = new WebAssembly.Instance(new WebAssembly.Module(wasmBytes), {}).exports;
const enc = new TextEncoder();
const dec = new TextDecoder();
const MD = readFileSync(new URL('../tests/fixtures/data.md', import.meta.url), 'utf8');
const REFTYPE = ['shortcut', 'collapsed', 'full'];

function callWire() {
  const buf = enc.encode(MD);
  const inPtr = x.sparkdown_alloc(buf.length);
  new Uint8Array(x.memory.buffer).set(buf, inPtr);
  const ptr = x.sparkdown_to_mdast_wire(inPtr, buf.length);
  const total = new DataView(x.memory.buffer).getUint32(ptr, true);
  return { ptr, total, inPtr, inLen: buf.length };
}
function freeWire(h) {
  x.sparkdown_free(h.ptr, 4 + h.total);
  x.sparkdown_free(h.inPtr, h.inLen);
}

// reader with toggles: buildPos (create position objects), buildStr (decode strings)
function read(buildPos, buildStr) {
  const h = callWire();
  const mem = x.memory.buffer;
  const dv = new DataView(mem);
  const base = h.ptr + 4;
  const u8 = new Uint8Array(mem, base, h.total);
  let p = 0;
  const u32 = () => { const v = dv.getUint32(base + p, true); p += 4; return v; };
  const str = () => {
    const hdr = u32();
    const n = hdr & 0x7fffffff;
    const end = p + n;
    let s = '';
    if (buildStr) s = hdr >>> 31 && n <= 512 ? String.fromCharCode.apply(null, u8.subarray(p, end)) : dec.decode(u8.subarray(p, end));
    p = end;
    return s;
  };
  const opt = () => { if (dv.getUint32(base + p, true) === 0xffffffff) { p += 4; return null; } return str(); };
  const kids = () => { const n = u32(); const a = new Array(n); for (let i = 0; i < n; i++) a[i] = node(); return a; };
  function node() {
    const tag = u8[p++];
    let position;
    if (buildPos) position = { start: { line: u32(), column: u32(), offset: u32() }, end: { line: u32(), column: u32(), offset: u32() } };
    else { p += 24; position = null; }
    switch (tag) {
      case 0: return { type: 'root', children: kids(), position };
      case 1: return { type: 'paragraph', children: kids(), position };
      case 2: { const depth = u8[p++]; return { type: 'heading', depth, children: kids(), position }; }
      case 3: return { type: 'blockquote', children: kids(), position };
      case 4: { const f = u8[p++]; const st = u32(); return { type: 'list', ordered: !!(f & 1), start: st === 0xffffffff ? null : st, spread: !!(f & 2), children: kids(), position }; }
      case 5: { const spread = !!u8[p++]; return { type: 'listItem', spread, checked: null, children: kids(), position }; }
      case 6: return { type: 'thematicBreak', position };
      case 7: { const lang = opt(); const meta = opt(); const value = str(); return { type: 'code', lang, meta, value, position }; }
      case 8: return { type: 'html', value: str(), position };
      case 9: return { type: 'text', value: str(), position };
      case 10: return { type: 'emphasis', children: kids(), position };
      case 11: return { type: 'strong', children: kids(), position };
      case 12: return { type: 'delete', children: kids(), position };
      case 13: return { type: 'inlineCode', value: str(), position };
      case 14: return { type: 'break', position };
      case 15: { const url = str(); const title = opt(); return { type: 'link', url, title, children: kids(), position }; }
      case 16: { const url = str(); const title = opt(); const alt = str(); return { type: 'image', url, title, alt, position }; }
      case 17: { const identifier = str(); const label = str(); const url = str(); const title = opt(); return { type: 'definition', identifier, label, url, title, position }; }
      case 18: { const identifier = str(); const label = str(); const rt = REFTYPE[u8[p++]]; return { type: 'linkReference', identifier, label, referenceType: rt, children: kids(), position }; }
      case 19: { const identifier = str(); const label = str(); const rt = REFTYPE[u8[p++]]; const alt = str(); return { type: 'imageReference', identifier, label, referenceType: rt, alt, position }; }
    }
  }
  const tree = node();
  freeWire(h);
  return tree;
}

function best(fn, iters = 200, trials = 15) {
  for (let i = 0; i < 50; i++) fn();
  let b = Infinity;
  for (let t = 0; t < trials; t++) { const s = performance.now(); for (let i = 0; i < iters; i++) fn(); b = Math.min(b, (performance.now() - s) / iters); }
  return b;
}

const wasmOnly = best(() => { const h = callWire(); freeWire(h); });
const full = best(() => read(true, true));
const noPos = best(() => read(false, true));
const noStr = best(() => read(true, false));
const skel = best(() => read(false, false));

const r = (label, ms) => console.log(`  ${label.padEnd(38)} ${ms.toFixed(3).padStart(7)} ms`);
console.log('\nwire profile — CommonMark spec (198 KB), best-of-15\n');
r('wasm only (parse+serialize+boundary)', wasmOnly);
r('full reader (pos + strings)', full);
r('  reader without position objects', noPos);
r('  reader without string decode', noStr);
r('  reader skeleton (no pos, no str)', skel);
console.log('\nderived:');
r('JS build total (full - wasm)', full - wasmOnly);
r('  cost of position objects', full - noPos);
r('  cost of string decode', full - noStr);
r('  cost of node/struct skeleton', skel - wasmOnly);
console.log();
