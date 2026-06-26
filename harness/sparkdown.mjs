// SPIKE: sparkdown as a unified-compatible parser plugin.
//
// This is the shape of the real npm package's JS glue. Using it makes sparkdown
// a drop-in replacement for `remark-parse`:
//
//   import { unified } from 'unified'
//   import sparkdown from '@momiji-rs/sparkdown/unified'   // <- this module
//   import remarkToc from 'remark-toc'
//   import remarkRehype from 'remark-rehype'
//   import rehypeStringify from 'rehype-stringify'
//
//   const html = String(await unified()
//     .use(sparkdown)            // parse markdown -> mdast IN WASM
//     .use(remarkToc)            // any mdast transform plugin
//     .use(remarkRehype)         // mdast -> hast
//     .use(rehypeStringify)      // hast -> HTML
//     .process(markdown))
//
// The parser is synchronous (unified requires it), so the wasm module is
// instantiated synchronously once at import. Node compiles wasm of any size
// synchronously; a browser build would top-level-await an async instantiate.

import { readFileSync } from 'node:fs';

const bytes = readFileSync(new URL('./sparkdown.wasm', import.meta.url));
const instance = new WebAssembly.Instance(new WebAssembly.Module(bytes), {});
const x = instance.exports;
const enc = new TextEncoder();
const dec = new TextDecoder();

/** Parse markdown → mdast tree by calling the wasm core. */
function parseToMdast(md) {
  const buf = enc.encode(md);
  const inPtr = x.sparkdown_alloc(buf.length);
  new Uint8Array(x.memory.buffer).set(buf, inPtr);
  const ptr = x.sparkdown_to_mdast_json(inPtr, buf.length);
  const len = new DataView(x.memory.buffer).getUint32(ptr, true);
  const json = dec.decode(new Uint8Array(x.memory.buffer, ptr + 4, len));
  x.sparkdown_free(ptr, 4 + len);
  x.sparkdown_free(inPtr, buf.length);
  return JSON.parse(json);
}

// Like parseToMdast, but with the opt-in extension flags bitmask (matching
// sparkdown_to_html_opts). The JSON path carries full inline children, so it is
// what the directive/deflist mdast gates check against the ecosystem trees.
function parseToMdastJson(md, flags = 0) {
  const buf = enc.encode(md);
  const inPtr = x.sparkdown_alloc(buf.length);
  new Uint8Array(x.memory.buffer).set(buf, inPtr);
  const ptr = x.sparkdown_to_mdast_json_opts(inPtr, buf.length, flags);
  const len = new DataView(x.memory.buffer).getUint32(ptr, true);
  const json = dec.decode(new Uint8Array(x.memory.buffer, ptr + 4, len));
  x.sparkdown_free(ptr, 4 + len);
  x.sparkdown_free(inPtr, buf.length);
  return JSON.parse(json);
}

/**
 * unified plugin: install sparkdown as the processor's parser.
 * (Mirrors how `remark-parse` attaches `this.parser`.)
 */
export default function remarkSparkdown() {
  this.parser = (doc) => parseToMdast(doc);
}

// --- route A: binary wire format (no JSON) ----------------------------------
// Reads the compact little-endian wire (see src/ast.rs `to_mdast_wire`) directly
// out of wasm linear memory and builds the same remark-shaped plain-object tree
// — no JSON serialize in Rust, no whole-buffer decode, no JSON.parse.
const REFTYPE = ['shortcut', 'collapsed', 'full'];

/**
 * Parse markdown → mdast tree via the binary wire boundary. `flags` (a bitmask
 * matching `sparkdown_to_html_opts`; bit 8 = frontmatter) enables opt-in grammar
 * extensions; 0 (the default) is pure CommonMark via the plain wire export.
 */
function parseToMdastWire(md, flags = 0) {
  const buf = enc.encode(md);
  const inPtr = x.sparkdown_alloc(buf.length);
  new Uint8Array(x.memory.buffer).set(buf, inPtr);
  const ptr = flags
    ? x.sparkdown_to_mdast_wire_opts(inPtr, buf.length, flags)
    : x.sparkdown_to_mdast_wire(inPtr, buf.length);

  const mem = x.memory.buffer;
  const dv = new DataView(mem);
  const total = dv.getUint32(ptr, true);
  const base = ptr + 4;
  const u8 = new Uint8Array(mem, base, total);
  let p = 0;

  const u32 = () => {
    const v = dv.getUint32(base + p, true);
    p += 4;
    return v;
  };
  // String reader: the length's high bit (set by Rust) flags an ASCII string,
  // so we take the fast `fromCharCode` path with no scan; fall back to
  // TextDecoder for UTF-8 (and for long ASCII, to avoid apply's arg limit).
  const str = () => {
    const hdr = u32();
    const n = hdr & 0x7fffffff;
    const end = p + n;
    const s =
      hdr >>> 31 && n <= 512
        ? String.fromCharCode.apply(null, u8.subarray(p, end))
        : dec.decode(u8.subarray(p, end));
    p = end;
    return s;
  };
  const opt = () => {
    if (dv.getUint32(base + p, true) === 0xffffffff) {
      p += 4;
      return null;
    }
    return str();
  };
  const kids = () => {
    const n = u32();
    const a = new Array(n);
    for (let i = 0; i < n; i++) a[i] = node();
    return a;
  };
  // Directive attributes: u32 count, then count (key, value) string pairs.
  const attrs = () => {
    const n = u32();
    const o = {};
    for (let i = 0; i < n; i++) {
      const k = str();
      o[k] = str();
    }
    return o;
  };

  function node() {
    const tag = u8[p++];
    const position = {
      start: { line: u32(), column: u32(), offset: u32() },
      end: { line: u32(), column: u32(), offset: u32() },
    };
    switch (tag) {
      case 0: return { type: 'root', children: kids(), position };
      case 1: return { type: 'paragraph', children: kids(), position };
      case 2: { const depth = u8[p++]; return { type: 'heading', depth, children: kids(), position }; }
      case 3: return { type: 'blockquote', children: kids(), position };
      case 4: {
        const flags = u8[p++];
        const st = u32();
        return { type: 'list', ordered: !!(flags & 1), start: st === 0xffffffff ? null : st, spread: !!(flags & 2), children: kids(), position };
      }
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
      case 20: return { type: 'yaml', value: str(), position };
      case 21: return { type: 'toml', value: str(), position };
      case 22: { const identifier = str(); const label = str(); return { type: 'footnoteDefinition', identifier, label, children: kids(), position }; }
      case 23: { const identifier = str(); const label = str(); return { type: 'footnoteReference', identifier, label, position }; }
      case 24: return { type: 'defList', children: kids(), position };
      case 25: return { type: 'defListTerm', children: kids(), position };
      case 26: { const spread = !!u8[p++]; return { type: 'defListDescription', spread, children: kids(), position }; }
      // NB: on the wire path a textDirective's [label] children are emitted empty
      // (the streaming sink has no scratch to re-tokenize the label); use
      // parseToMdastJson(md, flags) for the full inline children.
      case 27: { const name = str(); const attributes = attrs(); return { type: 'textDirective', name, attributes, children: kids(), position }; }
      case 28: { const name = str(); const attributes = attrs(); return { type: 'leafDirective', name, attributes, children: kids(), position }; }
      case 29: { const name = str(); const attributes = attrs(); return { type: 'containerDirective', name, attributes, children: kids(), position }; }
      case 30: return { type: 'paragraph', data: { directiveLabel: true }, children: kids(), position };
      default: throw new Error('bad wire tag ' + tag);
    }
  }

  const tree = node();
  x.sparkdown_free(ptr, 4 + total);
  x.sparkdown_free(inPtr, buf.length);
  return tree;
}

export { parseToMdast, parseToMdastJson, parseToMdastWire };
