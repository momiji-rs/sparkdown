import { readFileSync } from 'node:fs';
const wasmBytes = readFileSync(new URL('./sparkdown-named.wasm', import.meta.url));
const { instance } = await WebAssembly.instantiate(wasmBytes, {});
const ex = instance.exports;
const input = new TextEncoder().encode(readFileSync(new URL('../tests/fixtures/data.md', import.meta.url), 'utf8'));
let sink = 0;
for (let i = 0; i < +(process.argv[2] || 30000); i++) {
  const inPtr = ex.sparkdown_alloc(input.length);
  new Uint8Array(ex.memory.buffer).set(input, inPtr);
  const ptr = ex.sparkdown_to_mdast_wire_fast_opts(inPtr, input.length, 0);
  const total = new DataView(ex.memory.buffer).getUint32(ptr, true);
  sink ^= total;
  ex.sparkdown_free(ptr, 4 + total);
  ex.sparkdown_free(inPtr, input.length);
}
console.error('sink', sink);
