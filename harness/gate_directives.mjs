// Alignment gate (task #10): sparkdown's native directives grammar vs
// remark-directive. Directives have no canonical HTML (the rehype handler is
// userland), so the gate-able output is the mdast: textDirective / leafDirective
// / containerDirective with `name` + `attributes`. Run: node gate_directives.mjs

import { unified } from 'unified';
import remarkParse from 'remark-parse';
import remarkDirective from 'remark-directive';
import { runGate, eqMdast } from './gate_harness.mjs';
import { parseToMdastJson } from './sparkdown.mjs';

const DIRECTIVES = 8192; // flag bit 13

const CORPUS = [
  // text directives (inline)
  ':name[label]{#id .cls key=val}\n',
  'a :em[x *y*] b\n',
  ':a\n',
  'text :a then\n',
  ':foo: bar\n', // trailing colon → not a directive (emoji-shaped)
  ':x{.a .b #i k=v k2="q w" bare}\n',
  ':x{#a #b .c .d}\n', // id last-wins, classes accumulate
  'see http://x.com\n', // `://` is not a directive
  // leaf directives
  '::leaf[lab]{.warn}\n',
  '::leaf\n',
  // container directives
  ':::note\n## Title\n\ntext\n:::\n',
  ':::card{#x .a}\nbody\n:::\n',
  ':::note[The *title*]\nbody\n:::\n', // container label → directiveLabel paragraph
  ':::w\n:::\n', // empty container
];

const ref = unified().use(remarkParse).use(remarkDirective);

console.log('directives alignment gate — sparkdown (native) vs remark-directive\n');
const g1 = runGate({
  label: 'Gate 1 — mdast structural (ignore position)',
  items: CORPUS,
  a: (md) => parseToMdastJson(md, DIRECTIVES),
  b: (md) => ref.parse(md),
  eq: eqMdast({ dropPos: true }),
  limit: 14,
});
const g2 = runGate({
  label: 'Gate 2 — mdast with position',
  items: CORPUS,
  a: (md) => parseToMdastJson(md, DIRECTIVES),
  b: (md) => ref.parse(md),
  eq: eqMdast({ dropPos: false }),
  limit: 14,
});

process.exit(g1.ok && g2.ok ? 0 : 1);
