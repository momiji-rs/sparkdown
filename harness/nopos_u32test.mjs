// Does direct byte-math beat DataView.getUint32 in the wire reader? u32() is
// called ~10k×/parse (child counts, string lengths, opt peeks). best-of-30 ×3.
import { readFileSync } from 'node:fs';
import { markdownToMdast } from 'satteri';

const wasmBytes = readFileSync(new URL('./sparkdown.wasm', import.meta.url));
const { instance } = await WebAssembly.instantiate(wasmBytes, {});
const ex = instance.exports;
const enc = new TextEncoder();
const dec = new TextDecoder();
const md = readFileSync(new URL('../tests/fixtures/data.md', import.meta.url), 'utf8');
const inputBytes = enc.encode(md);
const REFTYPE = ['shortcut', 'collapsed', 'full'];

function makeReader(byteMath) {
  return function read() {
    const inPtr = ex.sparkdown_alloc(inputBytes.length);
    new Uint8Array(ex.memory.buffer).set(inputBytes, inPtr);
    const ptr = ex.sparkdown_to_mdast_wire_nopos_opts(inPtr, inputBytes.length, 0);
    const mem = ex.memory.buffer;
    const dv = new DataView(mem);
    const total = dv.getUint32(ptr, true);
    const base = ptr + 4;
    const u8 = new Uint8Array(mem, base, total);
    let p = 0;
    const u32 = byteMath
      ? () => { const v = (u8[p] | (u8[p + 1] << 8) | (u8[p + 2] << 16) | (u8[p + 3] << 24)) >>> 0; p += 4; return v; }
      : () => { const v = dv.getUint32(base + p, true); p += 4; return v; };
    const peek = byteMath
      ? () => (u8[p] | (u8[p + 1] << 8) | (u8[p + 2] << 16) | (u8[p + 3] << 24)) >>> 0
      : () => dv.getUint32(base + p, true);
    const str = () => { const hdr = u32(); const n = hdr & 0x7fffffff; const end = p + n; const s = n === 0 ? '' : dec.decode(u8.subarray(p, end)); p = end; return s; };
    const opt = () => { if (peek() === 0xffffffff) { p += 4; return null; } return str(); };
    const kids = () => { const n = u32(); const a = new Array(n); for (let i = 0; i < n; i++) a[i] = node(); return a; };
    function node() {
      const tag = u8[p++];
      switch (tag) {
        case 0: return { type: 'root', children: kids() };
        case 1: return { type: 'paragraph', children: kids() };
        case 2: { const depth = u8[p++]; return { type: 'heading', depth, children: kids() }; }
        case 3: return { type: 'blockquote', children: kids() };
        case 4: { const f = u8[p++]; const st = u32(); return { type: 'list', ordered: !!(f & 1), start: st === 0xffffffff ? null : st, spread: !!(f & 2), children: kids() }; }
        case 5: { const spread = !!u8[p++]; const ck = u8[p++]; return { type: 'listItem', spread, checked: ck === 2 ? null : ck === 1, children: kids() }; }
        case 6: return { type: 'thematicBreak' };
        case 7: { const lang = opt(); const meta = opt(); const value = str(); return { type: 'code', lang, meta, value }; }
        case 8: return { type: 'html', value: str() };
        case 9: return { type: 'text', value: str() };
        case 10: return { type: 'emphasis', children: kids() };
        case 11: return { type: 'strong', children: kids() };
        case 12: return { type: 'delete', children: kids() };
        case 13: return { type: 'inlineCode', value: str() };
        case 14: return { type: 'break' };
        case 15: { const url = str(); const title = opt(); return { type: 'link', url, title, children: kids() }; }
        case 16: { const url = str(); const title = opt(); const alt = str(); return { type: 'image', url, title, alt }; }
        case 17: { const identifier = str(); const label = str(); const url = str(); const title = opt(); return { type: 'definition', identifier, label, url, title }; }
        case 18: { const identifier = str(); const label = str(); const rt = REFTYPE[u8[p++]]; return { type: 'linkReference', identifier, label, referenceType: rt, children: kids() }; }
        case 19: { const identifier = str(); const label = str(); const rt = REFTYPE[u8[p++]]; const alt = str(); return { type: 'imageReference', identifier, label, referenceType: rt, alt }; }
        case 31: { const ncols = u32(); const align = new Array(ncols); for (let i = 0; i < ncols; i++) align[i] = [null, 'left', 'right', 'center'][u8[p++]]; return { type: 'table', align, children: kids() }; }
        case 32: return { type: 'tableRow', children: kids() };
        case 33: return { type: 'tableCell', children: kids() };
        default: throw new Error('tag ' + tag);
      }
    }
    const tree = node();
    ex.sparkdown_free(ptr, 4 + total);
    ex.sparkdown_free(inPtr, inputBytes.length);
    return tree;
  };
}

const dvRead = makeReader(false);
const byteRead = makeReader(true);
// correctness
if (JSON.stringify(dvRead()) !== JSON.stringify(byteRead())) { console.error('MISMATCH'); process.exit(1); }
const satFeat = { features: { gfm: false, frontmatter: false } };
const bench = (fn) => { for (let i = 0; i < 30; i++) fn(); let b = Infinity; for (let t = 0; t < 30; t++) { const t0 = performance.now(); for (let i = 0; i < 30; i++) fn(); const ms = (performance.now() - t0) / 30; if (ms < b) b = ms; } return b; };
for (let trial = 1; trial <= 3; trial++) {
  console.log(`trial ${trial}: dataview ${bench(dvRead).toFixed(3)}  bytemath ${bench(byteRead).toFixed(3)}  satteri ${bench(() => markdownToMdast(md, satFeat)).toFixed(3)} ms`);
}
