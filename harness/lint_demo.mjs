// SPIKE: position-reading plugins. remark-lint rules report `line:column` taken
// from each node's unist `position`. If sparkdown's positions are right, the lint
// diagnostics on our tree match remark's exactly — proving position works.
//
//   sparkdown (wasm) → mdast (+position)
//     → remark-lint + remark-lint-no-multiple-toplevel-headings
//                    + remark-lint-maximum-heading-length
//   compare file.messages (which embed line:col) vs the same on remark's tree.

import { readFileSync } from 'node:fs';
import { fromMarkdown } from 'mdast-util-from-markdown';
import { unified } from 'unified';
import remarkLint from 'remark-lint';
import noMultipleH1 from 'remark-lint-no-multiple-toplevel-headings';
import maxHeadingLength from 'remark-lint-maximum-heading-length';
import { VFile } from 'vfile';
import { visit } from 'unist-util-visit';

const wasmBytes = readFileSync(new URL('./sparkdown.wasm', import.meta.url));
const { instance } = await WebAssembly.instantiate(wasmBytes, {});
const x = instance.exports;
const enc = new TextEncoder();
const dec = new TextDecoder();
function sparkdownToMdast(md) {
  const bytes = enc.encode(md);
  const inPtr = x.sparkdown_alloc(bytes.length);
  new Uint8Array(x.memory.buffer).set(bytes, inPtr);
  const ptr = x.sparkdown_to_mdast_json(inPtr, bytes.length);
  const len = new DataView(x.memory.buffer).getUint32(ptr, true);
  const json = dec.decode(new Uint8Array(x.memory.buffer, ptr + 4, len));
  x.sparkdown_free(ptr, 4 + len);
  x.sparkdown_free(inPtr, bytes.length);
  return JSON.parse(json);
}

const processor = unified().use(remarkLint).use(noMultipleH1).use(maxHeadingLength);
async function lint(tree, md) {
  const file = new VFile({ value: md });
  await processor.run(structuredClone(tree), file);
  return file.messages.map(String).sort();
}

const md = `# First top-level title

## A normal section

# A second top-level title, which the linter forbids

## This section heading is deliberately far too long to stay under the limit rule
`;

const ourTree = sparkdownToMdast(md);
const refTree = fromMarkdown(md);

// Show that positions are actually present and accurate on our headings.
console.log('\n=== position-reading lint plugins on sparkdown mdast ===\n');
console.log('our heading positions (line:col):');
visit(ourTree, 'heading', (h) => {
  const p = h.position?.start;
  const text = h.children?.map((c) => c.value || '').join('');
  console.log(`  ${p ? `${p.line}:${p.column}` : 'NONE'}  h${h.depth}  ${JSON.stringify(text.slice(0, 32))}`);
});

const ourMsgs = await lint(ourTree, md);
const refMsgs = await lint(refTree, md);

console.log('\nlint diagnostics from OUR tree (line:col come from our positions):');
for (const m of ourMsgs) console.log('  ' + m);

console.log('\n--- match vs remark tree ---');
console.log('  diagnostics identical:', JSON.stringify(ourMsgs) === JSON.stringify(refMsgs) ? 'YES ✅' : 'NO ❌');
if (JSON.stringify(ourMsgs) !== JSON.stringify(refMsgs)) {
  console.log('  ours:', ourMsgs);
  console.log('  ref :', refMsgs);
}

// --- generalize: heading-position accuracy across the whole corpus ---
const corpus = JSON.parse(readFileSync(new URL('./sparkdown-mdast.json', import.meta.url)));
let hdgTotal = 0;
let hdgMatch = 0;
const mism = [];
for (const e of corpus) {
  const ref = fromMarkdown(e.markdown);
  const ours = e.mdast;
  const refH = [];
  visit(ref, 'heading', (h) => refH.push(h.position.start));
  const ourH = [];
  visit(ours, 'heading', (h) => ourH.push(h.position?.start));
  for (let i = 0; i < refH.length; i++) {
    hdgTotal++;
    const a = ourH[i];
    const b = refH[i];
    if (a && a.line === b.line && a.column === b.column && a.offset === b.offset) hdgMatch++;
    else if (mism.length < 8) mism.push({ ex: e.example, ours: a, ref: b });
  }
}
console.log('\n--- heading-position accuracy across 652 examples ---');
console.log(`  headings with exact (line,col,offset) match vs remark: ${hdgMatch} / ${hdgTotal} (${((100 * hdgMatch) / hdgTotal).toFixed(1)}%)`);
if (mism.length) {
  console.log('  sample mismatches:');
  for (const m of mism) console.log(`    ex ${m.ex}: ours=${JSON.stringify(m.ours)} ref=${JSON.stringify(m.ref)}`);
}
console.log();
