// Benchmark the REAL no-position wire (sparkdown_to_mdast_wire_nopos_opts) vs the
// position-on wire, satteri (lazy), and remark. The no-position reader skips the
// 24 position bytes/node and builds no position object.
import { readFileSync } from 'node:fs';
import { markdownToMdast } from 'satteri';
import { fromMarkdown } from 'mdast-util-from-markdown';

const wasmBytes = readFileSync(new URL('./sparkdown.wasm', import.meta.url));
const { instance } = await WebAssembly.instantiate(wasmBytes, {});
const ex = instance.exports;
const enc = new TextEncoder();
const dec = new TextDecoder();
const REFTYPE = ['shortcut', 'collapsed', 'full'];

// withPos=true → call the position wire and read position; false → no-position wire.
function makeRead(withPos) {
  return function read(md) {
    const buf = enc.encode(md);
    const inPtr = ex.sparkdown_alloc(buf.length);
    new Uint8Array(ex.memory.buffer).set(buf, inPtr);
    const ptr = withPos
      ? ex.sparkdown_to_mdast_wire(inPtr, buf.length)
      : ex.sparkdown_to_mdast_wire_nopos_opts(inPtr, buf.length, 0);
    const mem = ex.memory.buffer;
    const dv = new DataView(mem);
    const total = dv.getUint32(ptr, true);
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
    const kids = () => { const n = u32(); const a = new Array(n); for (let i = 0; i < n; i++) a[i] = node(); return a; };
    const attrs = () => { const n = u32(); const o = {}; for (let i = 0; i < n; i++) { const k = str(); o[k] = str(); } return o; };
    function node() {
      const tag = u8[p++];
      const position = withPos
        ? { start: { line: u32(), column: u32(), offset: u32() }, end: { line: u32(), column: u32(), offset: u32() } }
        : undefined;
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
        case 18: { const identifier = str(); const label = str(); const referenceType = REFTYPE[u8[p++]]; return { type: 'linkReference', identifier, label, referenceType, children: kids(), position }; }
        case 19: { const identifier = str(); const label = str(); const referenceType = REFTYPE[u8[p++]]; const alt = str(); return { type: 'imageReference', identifier, label, referenceType, alt, position }; }
        default: throw new Error('tag ' + tag);
      }
    }
    const tree = node();
    ex.sparkdown_free(ptr, 4 + total);
    ex.sparkdown_free(inPtr, buf.length);
    return tree;
  };
}

const withPos = makeRead(true);
const noPos = makeRead(false);
const md = readFileSync(new URL('../tests/fixtures/data.md', import.meta.url), 'utf8');
const satFeat = { features: { gfm: false, frontmatter: false } };
const bench = (fn) => { for (let i = 0; i < 25; i++) fn(); let b = Infinity; for (let t = 0; t < 20; t++) { const t0 = performance.now(); for (let i = 0; i < 25; i++) fn(); const ms = (performance.now() - t0) / 25; if (ms < b) b = ms; } return b; };

// sanity: same structure (minus position)
const a = JSON.stringify(withPos(md), (k, v) => (k === 'position' ? undefined : v));
const b = JSON.stringify(noPos(md));
console.log('nopos == pos minus position:', a === b);

console.log('\nmd → mdast (materialized), 200 KB spec, best-of-20:\n');
for (const [n, fn] of [
  ['sparkdown toMdast — WITH position', () => withPos(md)],
  ['sparkdown toMdast — NO position (new)', () => noPos(md)],
  ['satteri markdownToMdast (lazy handle)', () => markdownToMdast(md, satFeat)],
  ['remark-parse (mdast-util-from-markdown)', () => fromMarkdown(md)],
]) {
  console.log(`  ${n.padEnd(40)} ${bench(fn).toFixed(3)} ms`);
}
