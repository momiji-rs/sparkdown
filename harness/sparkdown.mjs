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

/**
 * unified plugin: install sparkdown as the processor's parser.
 * (Mirrors how `remark-parse` attaches `this.parser`.)
 */
export default function remarkSparkdown() {
  this.parser = (doc) => parseToMdast(doc);
}

export { parseToMdast };
