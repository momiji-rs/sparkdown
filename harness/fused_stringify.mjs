// PROTOTYPE A: a fused mdast -> HTML stringifier (one walk, no intermediate hast
// tree), a drop-in for stages 3+4 (remark-rehype + rehype-stringify) of the
// unified pipeline. Keeps FULL plain-object compatibility: it walks the same
// mdast plain objects, honours plugin `data.hName/hProperties`, and must produce
// byte-identical HTML to `toHtml(toHast(mdast))`.
//
// Replicates mdast-util-to-hast's handlers + wrap() newline rules and
// hast-util-to-html's escaping (verified against both on all 652 examples).

import { readFileSync } from 'node:fs';
import { pathToFileURL } from 'node:url';
import { toHast } from 'mdast-util-to-hast';
import { toHtml } from 'hast-util-to-html';
import { normalizeUri } from 'micromark-util-sanitize-uri';
import { parseToMdastWire } from './sparkdown.mjs';

const escText = (s) => (/[&<]/.test(s) ? s.replace(/[&<]/g, (c) => (c === '&' ? '&#x26;' : '&#x3C;')) : s);
const escAttr = (s) => (/[&"]/.test(s) ? s.replace(/[&"]/g, (c) => (c === '&' ? '&#x26;' : '&#x22;')) : s);

// ---- the fused stringifier --------------------------------------------------
export function mdastToHtml(tree) {
  const defs = new Map();
  (function collect(n) {
    if (n.type === 'definition') {
      const id = String(n.identifier).toUpperCase(); // first definition wins
      if (!defs.has(id)) defs.set(id, n);
    }
    const c = n.children;
    if (c) for (let i = 0; i < c.length; i++) collect(c[i]);
  })(tree);

  // serialize a hast-style properties object (insertion order = output order)
  function attrs(props) {
    let s = '';
    for (const k in props) {
      let v = props[k];
      if (v === false || v === null || v === undefined) continue;
      const name = k === 'className' ? 'class' : k;
      if (Array.isArray(v)) v = v.join(' ');
      if (v === true) s += ' ' + name;
      else s += ' ' + name + '="' + escAttr(String(v)) + '"';
    }
    return s;
  }
  // mdast-util-to-hast applyData: data.hName overrides tag, hProperties merge in.
  const tagOf = (node, base) => (node.data && node.data.hName) || base;
  const propsOf = (node, base) => (node.data && node.data.hProperties ? { ...base, ...node.data.hProperties } : base);

  function inline(nodes) {
    let s = '';
    for (let i = 0; i < nodes.length; i++) s += one(nodes[i]);
    return s;
  }

  // Nodes that produce NO hast output: dropped before wrap() so they don't add
  // stray separator newlines (mirrors mdast-util-to-hast's nullish filtering).
  const DROP = new Set(['definition', 'footnoteDefinition', 'yaml', 'toml']);
  const blockKids = (node) => node.children.filter((c) => !DROP.has(c.type));

  // wrap(): newlines between block children; loose adds surrounding newlines.
  function wrapBlocks(kidsRaw, loose) {
    const kids = kidsRaw.filter((c) => !DROP.has(c.type));
    let s = loose ? '\n' : '';
    for (let i = 0; i < kids.length; i++) {
      if (i) s += '\n';
      s += one(kids[i]);
    }
    if (loose && kids.length > 0) s += '\n';
    return s;
  }

  const listLoose = (list) => list.spread || list.children.some(itemLoose);
  const itemLoose = (n) => (n.spread === null || n.spread === undefined ? n.children.length > 1 : n.spread);

  function listItem(node, parent) {
    const loose = parent ? listLoose(parent) : itemLoose(node);
    const kids = blockKids(node);
    let inner = '';
    for (let i = 0; i < kids.length; i++) {
      const child = kids[i];
      const isP = child.type === 'paragraph';
      if (loose || i !== 0 || !isP) inner += '\n';
      inner += isP && !loose ? inline(child.children) : one(child);
    }
    const tail = kids[kids.length - 1];
    if (tail && (loose || tail.type !== 'paragraph')) inner += '\n';
    const props = propsOf(node, {});
    return '<' + tagOf(node, 'li') + attrs(props) + '>' + inner + '</' + tagOf(node, 'li') + '>';
  }

  function revert(node) {
    const t = node.referenceType;
    let suffix = ']';
    if (t === 'collapsed') suffix += '[]';
    else if (t === 'full') suffix += '[' + (node.label || node.identifier) + ']';
    if (node.type === 'imageReference') return escText('![' + node.alt + suffix);
    return '[' + inline(node.children) + suffix;
  }

  function one(n) {
    switch (n.type) {
      case 'root':
        return wrapBlocks(n.children, false);
      case 'text':
        return escText(n.value);
      case 'paragraph':
        return '<' + tagOf(n, 'p') + attrs(propsOf(n, {})) + '>' + inline(n.children) + '</' + tagOf(n, 'p') + '>';
      case 'heading': {
        const tag = tagOf(n, 'h' + n.depth);
        return '<' + tag + attrs(propsOf(n, {})) + '>' + inline(n.children) + '</' + tag + '>';
      }
      case 'blockquote':
        return '<blockquote' + attrs(propsOf(n, {})) + '>' + wrapBlocks(n.children, true) + '</blockquote>';
      case 'list': {
        const base = {};
        if (typeof n.start === 'number' && n.start !== 1) base.start = n.start;
        const tag = n.ordered ? 'ol' : 'ul';
        let s = '\n';
        for (let i = 0; i < n.children.length; i++) {
          if (i) s += '\n';
          s += listItem(n.children[i], n); // thread parent for looseness
        }
        if (n.children.length > 0) s += '\n';
        return '<' + tag + attrs(propsOf(n, base)) + '>' + s + '</' + tag + '>';
      }
      case 'listItem':
        return listItem(n, null);
      case 'thematicBreak':
        return '<hr' + attrs(propsOf(n, {})) + '>';
      case 'code': {
        const base = {};
        const lang = n.lang ? n.lang.split(/\s+/)[0] : '';
        if (lang) base.className = ['language-' + lang];
        const value = n.value ? n.value + '\n' : '';
        return '<pre><code' + attrs(propsOf(n, base)) + '>' + escText(value) + '</code></pre>';
      }
      case 'html':
        return n.value;
      case 'emphasis':
        return '<em>' + inline(n.children) + '</em>';
      case 'strong':
        return '<strong>' + inline(n.children) + '</strong>';
      case 'delete':
        return '<del>' + inline(n.children) + '</del>';
      case 'inlineCode':
        return '<code>' + escText(n.value.replace(/\r?\n|\r/g, ' ')) + '</code>';
      case 'break':
        return '<br>\n';
      case 'link': {
        const base = { href: normalizeUri(n.url) };
        if (n.title !== null && n.title !== undefined) base.title = n.title;
        return '<a' + attrs(propsOf(n, base)) + '>' + inline(n.children) + '</a>';
      }
      case 'image': {
        const base = { src: normalizeUri(n.url) };
        if (n.alt !== null && n.alt !== undefined) base.alt = n.alt;
        if (n.title !== null && n.title !== undefined) base.title = n.title;
        return '<img' + attrs(propsOf(n, base)) + '>';
      }
      case 'linkReference': {
        const d = defs.get(String(n.identifier).toUpperCase());
        if (!d) return revert(n);
        const base = { href: normalizeUri(d.url || '') };
        if (d.title !== null && d.title !== undefined) base.title = d.title;
        return '<a' + attrs(propsOf(n, base)) + '>' + inline(n.children) + '</a>';
      }
      case 'imageReference': {
        const d = defs.get(String(n.identifier).toUpperCase());
        if (!d) return revert(n);
        const base = { src: normalizeUri(d.url || ''), alt: n.alt };
        if (d.title !== null && d.title !== undefined) base.title = d.title;
        return '<img' + attrs(propsOf(n, base)) + '>';
      }
      case 'definition':
        return '';
      default:
        return '';
    }
  }

  return one(tree);
}

// ---- correctness + benchmark (only when run directly, not when imported) ----
if (import.meta.url === pathToFileURL(process.argv[1] || '').href) main();

function main() {
const corpus = JSON.parse(readFileSync(new URL('./sparkdown-mdast.json', import.meta.url), 'utf8'));
const ref = (md) => toHtml(toHast(parseToMdastWire(md), { allowDangerousHtml: true }), { allowDangerousHtml: true });

let pass = 0;
const fails = [];
for (const e of corpus) {
  const a = mdastToHtml(parseToMdastWire(e.markdown));
  const b = ref(e.markdown);
  if (a === b) pass++;
  else if (fails.length < 6) fails.push({ ex: e.example, got: a.slice(0, 90), want: b.slice(0, 90) });
}
console.log(`\nFused mdast→HTML vs remark-rehype+rehype-stringify, 652 examples:`);
console.log(`  byte-identical: ${pass}/${corpus.length} ${pass === corpus.length ? '✅' : '❌'}`);
for (const f of fails) {
  console.log(`  ex#${f.ex}\n    got : ${JSON.stringify(f.got)}\n    want: ${JSON.stringify(f.want)}`);
}

if (pass === corpus.length) {
  const LARGE = readFileSync(new URL('../tests/fixtures/data.md', import.meta.url), 'utf8');
  const tree = parseToMdastWire(LARGE);
  const best = (fn, it, tr = 15) => {
    for (let i = 0; i < Math.min(20, it); i++) fn();
    let b = Infinity;
    for (let t = 0; t < tr; t++) { const s = performance.now(); for (let i = 0; i < it; i++) fn(); b = Math.min(b, (performance.now() - s) / it); }
    return b;
  };
  const tToHast = best(() => toHast(tree, { allowDangerousHtml: true }), 100);
  const h = toHast(tree, { allowDangerousHtml: true });
  const tToHtml = best(() => toHtml(h, { allowDangerousHtml: true }), 100);
  const tFused = best(() => mdastToHtml(tree), 100);
  const r = (l, ms) => console.log(`  ${l.padEnd(40)} ${ms.toFixed(2).padStart(7)} ms`);
  console.log('\nStages 3+4 on the 198 KB spec tree (best-of-15):');
  r('mdast→hast (mdast-util-to-hast)', tToHast);
  r('hast→HTML  (hast-util-to-html)', tToHtml);
  r('  subtotal (current stages 3+4)', tToHast + tToHtml);
  r('FUSED mdast→HTML (one walk) ★', tFused);
  console.log(`  → stages 3+4: ${(tToHast + tToHtml).toFixed(2)} ms  →  ${tFused.toFixed(2)} ms  (${((tToHast + tToHtml) / tFused).toFixed(2)}× faster)`);
}
}
