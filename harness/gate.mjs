// SPIKE: the compatibility GATE.
//
// One falsifiable metric for "100% compatible with mdast/remark":
//   our mdast deep-equals mdast-util-from-markdown's mdast (ignoring `position`).
// If the trees are identical, EVERY downstream consumer (plugin, renderer, tool)
// behaves identically — there is no observable difference.
//
// This reports the current %, root-causes every failure into a bucket, and
// projects the exact path to 100%. Exit code is non-zero unless 100% (CI gate).
//
//   Gate 1 (product):      deep-equal IGNORING position   ← this file's headline
//   Gate 2 (gold standard): deep-equal INCLUDING position  ← also reported

import { readFileSync } from 'node:fs';
import { fromMarkdown } from 'mdast-util-from-markdown';

const data = JSON.parse(readFileSync(new URL('./sparkdown-mdast.json', import.meta.url)));

// Canonicalize: sort keys; optionally drop `position` and/or `spread`.
function canon(node, { dropPos = true, dropSpread = false } = {}) {
  const walk = (n) => {
    if (Array.isArray(n)) return n.map(walk);
    if (n && typeof n === 'object') {
      const out = {};
      for (const k of Object.keys(n).sort()) {
        if (k === 'position' && dropPos) continue;
        if (k === 'spread' && dropSpread) continue;
        if (n[k] === undefined) continue;
        out[k] = walk(n[k]);
      }
      return out;
    }
    return n;
  };
  return JSON.stringify(walk(node));
}

// Does a tree contain reference-model nodes (which we resolve inline + drop)?
function hasRefNodes(tree) {
  let found = false;
  const w = (n) => {
    if (!n || typeof n !== 'object') return;
    if (n.type === 'definition' || n.type === 'linkReference' || n.type === 'imageReference')
      found = true;
    (n.children || []).forEach(w);
  };
  w(tree);
  return found;
}

// First differing path between two cleaned trees (for sampling "other").
function firstDiff(a, b, path = '$') {
  if (typeof a !== typeof b) return { path, a, b };
  if (a && b && typeof a === 'object') {
    if (Array.isArray(a) !== Array.isArray(b)) return { path, a: typeofLabel(a), b: typeofLabel(b) };
    const keys = new Set([...Object.keys(a), ...Object.keys(b)]);
    for (const k of keys) {
      if (k === 'position') continue;
      const d = firstDiff(a[k], b[k], `${path}.${k}`);
      if (d) return d;
    }
    return null;
  }
  return a === b ? null : { path, a, b };
}
const typeofLabel = (v) => (Array.isArray(v) ? 'array' : typeof v);

const N = data.length;
let passNoPos = 0;
let passWithPos = 0;
const buckets = { spread: [], reference: [], other: [] };
const otherSamples = [];

for (const e of data) {
  const ref = fromMarkdown(e.markdown);
  const ours = e.mdast;

  if (canon(ours, { dropPos: false }) === canon(ref, { dropPos: false })) passWithPos++;
  if (canon(ours) === canon(ref)) {
    passNoPos++;
    continue;
  }

  // Classify the failure.
  const spreadOnly =
    canon(ours, { dropSpread: true }) === canon(ref, { dropSpread: true });
  if (spreadOnly) buckets.spread.push(e.example);
  else if (hasRefNodes(ref)) buckets.reference.push(e.example);
  else {
    buckets.other.push(e.example);
    if (otherSamples.length < 4) {
      const d = firstDiff(JSON.parse(canon(ours)), JSON.parse(canon(ref)));
      otherSamples.push({ ex: e.example, section: e.section, md: e.markdown, diff: d });
    }
  }
}

const pct = (n) => ((100 * n) / N).toFixed(1);
console.log(`\n=== mdast compatibility gate — ${N} CommonMark examples ===\n`);
console.log(`  Gate 1  deep-equal IGNORING position : ${passNoPos}/${N}  (${pct(passNoPos)}%)`);
console.log(`  Gate 2  deep-equal INCLUDING position: ${passWithPos}/${N}  (${pct(passWithPos)}%)`);

const fail = N - passNoPos;
console.log(`\n  Gate 1 failures: ${fail}, by root cause:`);
console.log(`    reference-model (${buckets.reference.length})  emit definition/linkReference/imageReference nodes`);
console.log(`    spread          (${buckets.spread.length})  list/listItem spread granularity`);
console.log(`    other           (${buckets.other.length})  genuine bugs — investigate`);

console.log(`\n  Path to Gate 1 = 100%:`);
let running = passNoPos;
const step = (label, n) => {
  running += n;
  console.log(`    fix ${label.padEnd(16)} +${String(n).padStart(3)}  →  ${pct(running)}%`);
};
step('spread', buckets.spread.length);
step('reference-model', buckets.reference.length);
step('other (bugs)', buckets.other.length);

if (otherSamples.length) {
  console.log(`\n  "other" samples (the real bugs to fix):`);
  for (const s of otherSamples) {
    console.log(`    ex ${s.ex} [${s.section}] ${JSON.stringify(s.md.slice(0, 48))}`);
    if (s.diff) console.log(`      first diff at ${s.diff.path}: ours=${JSON.stringify(s.diff.a)} ref=${JSON.stringify(s.diff.b)}`);
  }
}

if (buckets.other.length) console.log(`\n  other examples: ${buckets.other.join(', ')}`);
console.log();

process.exit(passNoPos === N ? 0 : 1);
