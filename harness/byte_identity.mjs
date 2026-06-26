// How byte-identical is the Rust render_mdast (via wasm) to the JS unified output
// (mdast-util-to-hast + hast-util-to-html)? Scopes the productization of the render.
import { readFileSync } from 'node:fs';
import { toHast } from 'mdast-util-to-hast';
import { toHtml } from 'hast-util-to-html';

const wasmBytes = readFileSync(new URL('./sparkdown.wasm', import.meta.url));
const { instance } = await WebAssembly.instantiate(wasmBytes, {});
const ex = instance.exports;
const enc = new TextEncoder();
const dec = new TextDecoder();
function viaMdast(md) {
  const input = enc.encode(md);
  const p = ex.sparkdown_alloc(input.length);
  new Uint8Array(ex.memory.buffer).set(input, p);
  const out = ex.sparkdown_to_html_via_mdast_opts(p, input.length, 0);
  const buf = ex.memory.buffer;
  const len = new DataView(buf).getUint32(out, true);
  const s = dec.decode(new Uint8Array(buf, out + 4, len));
  ex.sparkdown_free(p, input.length);
  ex.sparkdown_free(out, 4 + len);
  return s;
}

const data = JSON.parse(readFileSync(new URL('./sparkdown-mdast.json', import.meta.url), 'utf8'));
let match = 0;
const fails = [];
for (const e of data) {
  const rust = viaMdast(e.markdown);
  const js = toHtml(toHast(e.mdast)) + '\n'; // rehype-stringify appends a trailing newline
  if (rust === js) match++;
  else if (fails.length < 6) fails.push({ ex: e.example, rust: rust.slice(0, 70), js: js.slice(0, 70) });
}
console.log(`\nrender_mdast == (mdast-util-to-hast + hast-util-to-html): ${match}/${data.length} (${(100 * match / data.length).toFixed(1)}%)\n`);
for (const f of fails) {
  console.log(`  ex ${f.ex}:\n    rust: ${JSON.stringify(f.rust)}\n    js:   ${JSON.stringify(f.js)}`);
}
