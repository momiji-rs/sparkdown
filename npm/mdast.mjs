// @momiji-rs/sparkdown/mdast — markdown → mdast (unist) in WebAssembly: a fast,
// drop-in `remark-parse` replacement, plus a Rust `mdast → html` render. Same
// engine as @momiji-rs/sparkdown, compiled with the mdast + full-extension set.
//
//   import { unified } from 'unified'
//   import sparkdownParse from '@momiji-rs/sparkdown/mdast'  // the parser plugin
//   import remarkRehype from 'remark-rehype'
//   import rehypeStringify from 'rehype-stringify'
//
//   await sparkdownParse.ready                 // instantiate the wasm (browser)
//   const html = String(unified()
//     .use(sparkdownParse)                     // markdown → mdast IN WASM
//     .use(remarkRehype).use(rehypeStringify)  // any remark/rehype plugins
//     .processSync(markdown))
//
// Or directly: `toMdast(md, options)` → tree, `toHtml(md, options)` → HTML.
//
// The wasm module is base64-inlined (./mdast-wasm-inline.mjs), so the package is
// self-contained: Node, browsers, bundlers, Deno, Bun, and edge runtimes.

import { WASM_BASE64 } from "./mdast-wasm-inline.mjs";

function base64ToBytes(b64) {
  if (typeof Buffer !== "undefined") {
    return new Uint8Array(Buffer.from(b64, "base64"));
  }
  const bin = atob(b64);
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
  return bytes;
}

let wasm = null;
let initPromise = null;

function instantiateSync() {
  return new WebAssembly.Instance(new WebAssembly.Module(base64ToBytes(WASM_BASE64)), {})
    .exports;
}

/**
 * Synchronously instantiate the wasm module (idempotent); afterwards `toMdastSync`
 * / `toHtmlSync` and the unified parser work with no await. For Node / Bun / Deno /
 * edge — NOT the browser main thread (synchronous WebAssembly compilation is capped
 * at ~4 KB there); use `init()` / `ready` in browsers.
 */
export function initSync() {
  if (!wasm) wasm = instantiateSync();
  return wasm;
}

/** Instantiate the wasm module asynchronously (idempotent). Resolves to the exports. */
export function init() {
  if (wasm) return Promise.resolve(wasm);
  if (!initPromise) {
    initPromise = WebAssembly.instantiate(base64ToBytes(WASM_BASE64), {}).then(
      ({ instance }) => (wasm ??= instance.exports),
    );
  }
  return initPromise;
}

/** Resolves once the wasm is ready; then the `*Sync` forms (and the parser) work. */
export const ready = {
  then: (onFulfilled, onRejected) => init().then(() => onFulfilled?.(), onRejected),
};

const encoder = new TextEncoder();
const decoder = new TextDecoder();
const REFTYPE = ["shortcut", "collapsed", "full"];

// Default: pure CommonMark (a faithful `remark-parse` drop-in). Pass options to
// opt into grammar extensions, e.g. `toMdast(md, { tables: true, footnotes: true })`.
// Same bit layout as the other entries' `sparkdown_to_html_opts`.
function toFlags(options) {
  const o = options || {};
  return (
    (o.strikethrough ? 1 : 0) |
    (o.tasklist ? 2 : 0) |
    (o.autolink ? 4 : 0) |
    (o.tagfilter ? 8 : 0) |
    (o.tables ? 16 : 0) |
    (o.hardWraps ? 32 : 0) |
    (o.diagram ? 64 : 0) |
    (o.headingIds ? 128 : 0) |
    (o.frontmatter ? 256 : 0) |
    (o.footnotes ? 512 : 0) |
    (o.emoji ? 1024 : 0) |
    (o.externalLinks ? 2048 : 0) |
    (o.deflist ? 4096 : 0) |
    (o.directives ? 8192 : 0)
  );
}

// Read the compact binary wire (src/ast.rs `to_mdast_wire`) straight out of wasm
// linear memory into a remark-shaped plain-object tree — no JSON, no full-buffer
// decode. ASCII strings take a `fromCharCode` fast path; UTF-8 falls to TextDecoder.
function readWire(ex, markdown, flags, withPos) {
  const buf = encoder.encode(markdown);
  const inPtr = ex.sparkdown_alloc(buf.length);
  new Uint8Array(ex.memory.buffer).set(buf, inPtr);
  const ptr = !withPos
    ? ex.sparkdown_to_mdast_wire_nopos_opts(inPtr, buf.length, flags)
    : flags
      ? ex.sparkdown_to_mdast_wire_opts(inPtr, buf.length, flags)
      : ex.sparkdown_to_mdast_wire(inPtr, buf.length);

  const mem = ex.memory.buffer;
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
  const str = () => {
    const hdr = u32();
    const n = hdr & 0x7fffffff;
    const end = p + n;
    const s =
      hdr >>> 31 && n <= 512
        ? String.fromCharCode.apply(null, u8.subarray(p, end))
        : decoder.decode(u8.subarray(p, end));
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
    const position = withPos
      ? {
          start: { line: u32(), column: u32(), offset: u32() },
          end: { line: u32(), column: u32(), offset: u32() },
        }
      : undefined;
    switch (tag) {
      case 0: return { type: "root", children: kids(), position };
      case 1: return { type: "paragraph", children: kids(), position };
      case 2: { const depth = u8[p++]; return { type: "heading", depth, children: kids(), position }; }
      case 3: return { type: "blockquote", children: kids(), position };
      case 4: {
        const f = u8[p++];
        const st = u32();
        return { type: "list", ordered: !!(f & 1), start: st === 0xffffffff ? null : st, spread: !!(f & 2), children: kids(), position };
      }
      case 5: { const spread = !!u8[p++]; return { type: "listItem", spread, checked: null, children: kids(), position }; }
      case 6: return { type: "thematicBreak", position };
      case 7: { const lang = opt(); const meta = opt(); const value = str(); return { type: "code", lang, meta, value, position }; }
      case 8: return { type: "html", value: str(), position };
      case 9: return { type: "text", value: str(), position };
      case 10: return { type: "emphasis", children: kids(), position };
      case 11: return { type: "strong", children: kids(), position };
      case 12: return { type: "delete", children: kids(), position };
      case 13: return { type: "inlineCode", value: str(), position };
      case 14: return { type: "break", position };
      case 15: { const url = str(); const title = opt(); return { type: "link", url, title, children: kids(), position }; }
      case 16: { const url = str(); const title = opt(); const alt = str(); return { type: "image", url, title, alt, position }; }
      case 17: { const identifier = str(); const label = str(); const url = str(); const title = opt(); return { type: "definition", identifier, label, url, title, position }; }
      case 18: { const identifier = str(); const label = str(); const referenceType = REFTYPE[u8[p++]]; return { type: "linkReference", identifier, label, referenceType, children: kids(), position }; }
      case 19: { const identifier = str(); const label = str(); const referenceType = REFTYPE[u8[p++]]; const alt = str(); return { type: "imageReference", identifier, label, referenceType, alt, position }; }
      case 20: return { type: "yaml", value: str(), position };
      case 21: return { type: "toml", value: str(), position };
      case 22: { const identifier = str(); const label = str(); return { type: "footnoteDefinition", identifier, label, children: kids(), position }; }
      case 23: { const identifier = str(); const label = str(); return { type: "footnoteReference", identifier, label, position }; }
      case 24: return { type: "defList", children: kids(), position };
      case 25: return { type: "defListTerm", children: kids(), position };
      case 26: { const spread = !!u8[p++]; return { type: "defListDescription", spread, children: kids(), position }; }
      case 27: { const name = str(); const attributes = attrs(); return { type: "textDirective", name, attributes, children: kids(), position }; }
      case 28: { const name = str(); const attributes = attrs(); return { type: "leafDirective", name, attributes, children: kids(), position }; }
      case 29: { const name = str(); const attributes = attrs(); return { type: "containerDirective", name, attributes, children: kids(), position }; }
      case 30: return { type: "paragraph", data: { directiveLabel: true }, children: kids(), position };
      default: throw new Error("sparkdown/mdast: bad wire tag " + tag);
    }
  }

  const tree = node();
  ex.sparkdown_free(ptr, 4 + total);
  ex.sparkdown_free(inPtr, buf.length);
  return tree;
}

function renderViaMdast(ex, markdown, flags) {
  const input = encoder.encode(markdown);
  const inPtr = ex.sparkdown_alloc(input.length);
  new Uint8Array(ex.memory.buffer).set(input, inPtr);
  const outPtr = ex.sparkdown_to_html_via_mdast_opts(inPtr, input.length, flags);
  const b = ex.memory.buffer;
  const len = new DataView(b).getUint32(outPtr, true);
  const html = decoder.decode(new Uint8Array(b, outPtr + 4, len));
  ex.sparkdown_free(inPtr, input.length);
  ex.sparkdown_free(outPtr, 4 + len);
  return html;
}

/**
 * Parse `markdown` → an mdast (unist) tree. `options` opts into extensions.
 * Pass `{ position: false }` to skip unist `position` — ~30% faster and lighter
 * (uses the no-position wire), for plugins that do not read source positions.
 */
export async function toMdast(markdown, options) {
  return readWire(await init(), String(markdown), toFlags(options), options?.position !== false);
}

/** Synchronous parse — valid only after `await ready` / `initSync()`. */
export function toMdastSync(markdown, options) {
  if (!wasm) throw new Error("sparkdown/mdast: call initSync() or await ready before toMdastSync()");
  return readWire(wasm, String(markdown), toFlags(options), options?.position !== false);
}

/**
 * Render `markdown` → HTML through the in-wasm mdast → HTML pass — byte-identical
 * to `mdast-util-to-hast` + `hast-util-to-html` (the remark/rehype output shape).
 */
export async function toHtml(markdown, options) {
  return renderViaMdast(await init(), String(markdown), toFlags(options));
}

/** Synchronous render — valid only after `await ready` / `initSync()`. */
export function toHtmlSync(markdown, options) {
  if (!wasm) throw new Error("sparkdown/mdast: call initSync() or await ready before toHtmlSync()");
  return renderViaMdast(wasm, String(markdown), toFlags(options));
}

/**
 * unified plugin: install sparkdown as the processor's parser (drop-in for
 * `remark-parse`). The parser is synchronous, so the wasm must be instantiated
 * first — automatic in Node/Bun/Deno/edge; in the browser `await sparkdownParse.ready`
 * before `.processSync(...)`. `options` opts into grammar extensions.
 */
export default function sparkdownParse(options) {
  const flags = toFlags(options);
  const withPos = options?.position !== false;
  this.parser = (doc) => readWire(initSync(), String(doc), flags, withPos);
}

sparkdownParse.ready = ready;
