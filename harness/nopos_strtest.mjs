// String decoding is the cost. Isolate it (skip-decode vs decode) and race
// strategies: fromCharCode.apply (cap 512) vs always-TextDecoder vs a reused
// latin1 TextDecoder vs manual fromCharCode loop.
import { readFileSync } from 'node:fs';

const wasmBytes = readFileSync(new URL('./sparkdown.wasm', import.meta.url));
const { instance } = await WebAssembly.instantiate(wasmBytes, {});
const ex = instance.exports;
const enc = new TextEncoder();
const utf8 = new TextDecoder();
const latin1 = new TextDecoder('latin1');
const md = readFileSync(new URL('../tests/fixtures/data.md', import.meta.url), 'utf8');
const inputBytes = enc.encode(md);

// strImpl(u8, p, n, hdrHighBit) -> [string, newP]
const strategies = {
  skip: (u8, p, n) => ['', p + n],
  current: (u8, p, n, ascii) => { const end = p + n; const s = ascii && n <= 512 ? String.fromCharCode.apply(null, u8.subarray(p, end)) : utf8.decode(u8.subarray(p, end)); return [s, end]; },
  alwaysUtf8: (u8, p, n) => { const end = p + n; return [utf8.decode(u8.subarray(p, end)), end]; },
  latin1Ascii: (u8, p, n, ascii) => { const end = p + n; const s = ascii ? latin1.decode(u8.subarray(p, end)) : utf8.decode(u8.subarray(p, end)); return [s, end]; },
  utf8Subarray0: (u8, p, n) => { const end = p + n; return [end === p ? '' : utf8.decode(u8.subarray(p, end)), end]; },
};

function makeReader(strImpl) {
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
    const u32 = () => { const v = dv.getUint32(base + p, true); p += 4; return v; };
    const str = () => { const hdr = u32(); const n = hdr & 0x7fffffff; const r = strImpl(u8, p, n, hdr >>> 31); p = r[1]; return r[0]; };
    const opt = () => { if (dv.getUint32(base + p, true) === 0xffffffff) { p += 4; return null; } return str(); };
    const REFTYPE = ['shortcut', 'collapsed', 'full'];
    const kids = () => { const n = u32(); const a = new Array(n); for (let i = 0; i < n; i++) a[i] = node(); return a; };
    function node() {
      const tag = u8[p++];
      switch (tag) {
        case 0: return { type: 'root', children: kids() };
        case 1: return { type: 'paragraph', children: kids() };
        case 2: { const depth = u8[p++]; return { type: 'heading', depth, children: kids() }; }
        case 3: return { type: 'blockquote', children: kids() };
        case 4: { const f = u8[p++]; const st = u32(); return { type: 'list', ordered: !!(f & 1), start: st === 0xffffffff ? null : st, spread: !!(f & 2), children: kids() }; }
        case 5: { const spread = !!u8[p++]; return { type: 'listItem', spread, checked: null, children: kids() }; }
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
        default: throw new Error('tag ' + tag);
      }
    }
    const tree = node();
    ex.sparkdown_free(ptr, 4 + total);
    ex.sparkdown_free(inPtr, inputBytes.length);
    return tree;
  };
}

const bench = (fn) => { for (let i = 0; i < 30; i++) fn(); let b = Infinity; for (let t = 0; t < 25; t++) { const t0 = performance.now(); for (let i = 0; i < 30; i++) fn(); const ms = (performance.now() - t0) / 30; if (ms < b) b = ms; } return b; };

const { markdownToMdast } = await import('satteri');
const { fromMarkdown } = await import('mdast-util-from-markdown');
const satFeat = { features: { gfm: false, frontmatter: false } };

console.log('\nfull materialize, str strategy vs satteri/remark (200 KB spec, best-of-25):\n');
for (const name of ['current', 'alwaysUtf8', 'utf8Subarray0']) {
  console.log(`  sparkdown ${name.padEnd(16)} ${bench(makeReader(strategies[name])).toFixed(3)} ms`);
}
console.log(`  satteri markdownToMdast (lazy)  ${bench(() => markdownToMdast(md, satFeat)).toFixed(3)} ms`);
console.log(`  remark-parse                    ${bench(() => fromMarkdown(md)).toFixed(3)} ms`);
