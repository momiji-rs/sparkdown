// SPIKE: verify sparkdown's mdast plugs into the unified/remark ecosystem.
//
// For each of the 652 CommonMark examples, sparkdown emits an mdast tree
// (harness/sparkdown-mdast.json). This runs four escalating checks:
//
//   1. valid    — mdast-util-assert accepts the tree (structurally legal mdast)
//   2. shape    — tree deep-equals mdast-util-from-markdown's tree (ignoring
//                 `position`); i.e. we ARE the remark reference parser's output
//   3. rt-ref   — to-hast→to-html of our tree == same of the reference tree
//                 (semantically equivalent as far as the renderer cares)
//   4. rt-cmark — that HTML == cmark's HTML (the spec's expected output)
//
// Plus a real ecosystem util (mdast-util-to-string via unist-util-visit) is run
// on every tree to prove it is traversable by remark-style plugins.

import { readFileSync } from 'node:fs';
import { assert } from 'mdast-util-assert';
import { fromMarkdown } from 'mdast-util-from-markdown';
import { toHast } from 'mdast-util-to-hast';
import { toHtml } from 'hast-util-to-html';
import { toString } from 'mdast-util-to-string';
import { visit } from 'unist-util-visit';

const data = JSON.parse(readFileSync(new URL('./sparkdown-mdast.json', import.meta.url)));

// Deep-clone a tree dropping `position` and undefined fields, for shape compare.
function clean(node) {
  if (Array.isArray(node)) return node.map(clean);
  if (node && typeof node === 'object') {
    const out = {};
    for (const k of Object.keys(node).sort()) {
      if (k === 'position' || node[k] === undefined) continue;
      out[k] = clean(node[k]);
    }
    return out;
  }
  return node;
}
const canon = (n) => JSON.stringify(clean(n));

const html = (tree) =>
  toHtml(toHast(tree, { allowDangerousHtml: true }), { allowDangerousHtml: true });

// cmark emits a trailing newline; normalize that one difference away so the
// comparison is about structure, not the final \n.
const norm = (s) => s.replace(/\n+$/, '\n').trimEnd();

const checks = ['valid', 'shape', 'rt-ref', 'rt-cmark', 'plugin'];
const pass = Object.fromEntries(checks.map((c) => [c, 0]));
const fails = Object.fromEntries(checks.map((c) => [c, []]));
const record = (c, ok, ex) => (ok ? pass[c]++ : fails[c].push(ex));

for (const e of data) {
  const ours = e.mdast;
  const ref = fromMarkdown(e.markdown);

  let valid = true;
  try { assert(ours); } catch { valid = false; }
  record('valid', valid, e.example);

  record('shape', canon(ours) === canon(ref), e.example);

  let ourHtml = null;
  try { ourHtml = html(ours); } catch { /* renderer threw */ }
  const refHtml = html(ref);
  record('rt-ref', ourHtml !== null && norm(ourHtml) === norm(refHtml), e.example);
  record('rt-cmark', ourHtml !== null && norm(ourHtml) === norm(e.html), e.example);

  let pluginRan = false;
  try {
    let n = 0;
    visit(ours, () => { n++; });
    toString(ours); // a real ecosystem util consuming our tree
    pluginRan = n > 0 || e.markdown.trim() === '';
  } catch { pluginRan = false; }
  record('plugin', pluginRan, e.example);
}

const N = data.length;
const pct = (n) => ((100 * n) / N).toFixed(1).padStart(5);
console.log(`\nsparkdown mdast → unified/remark compatibility — ${N} CommonMark examples\n`);
console.log(`  check      pass / ${N}      %     what it proves`);
console.log('  ---------- ------------  ------  ----------------------------------------');
const desc = {
  valid: 'legal mdast (mdast-util-assert)',
  shape: 'identical tree to remark reference parser',
  'rt-ref': 'renders identically to the reference tree',
  'rt-cmark': "renders to cmark's exact expected HTML",
  plugin: 'traversable by remark-style plugins',
};
for (const c of checks) {
  console.log(`  ${c.padEnd(10)} ${String(pass[c]).padStart(4)} / ${N}   ${pct(pass[c])}%  ${desc[c]}`);
}

// Show a few failing examples per check, grouped, to guide the next iteration.
console.log('\n  first failing examples (by spec example #):');
for (const c of checks) {
  if (fails[c].length) {
    console.log(`    ${c.padEnd(9)} (${fails[c].length}): ${fails[c].slice(0, 15).join(', ')}${fails[c].length > 15 ? ' …' : ''}`);
  }
}

// Drill into one shape mismatch to make the diff concrete.
if (fails.shape.length) {
  const ex = fails.shape[0];
  const e = data.find((x) => x.example === ex);
  console.log(`\n  shape diff, example ${ex} — markdown: ${JSON.stringify(e.markdown)}`);
  console.log('    ours:', canon(e.mdast));
  console.log('    ref :', canon(fromMarkdown(e.markdown)));
}
console.log();
