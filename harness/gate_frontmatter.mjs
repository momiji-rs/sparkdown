// Alignment gate (task #8): sparkdown's native frontmatter mdast ≡
// remark-frontmatter's. Grammar-type gate — both sides produce mdast trees,
// compared with eqMdast. Two passes, matching the mdast-compat gate convention:
//   Gate 1 — structural (ignore position)
//   Gate 2 — byte-for-byte position parity
//
// Run: node gate_frontmatter.mjs

import { unified } from 'unified';
import remarkParse from 'remark-parse';
import remarkFrontmatter from 'remark-frontmatter';
import { runGate, eqMdast } from './gate_harness.mjs';
import { parseToMdastWire } from './sparkdown.mjs';

const FRONTMATTER = 256; // flag bit 8

// A representative corpus exercising the remark-frontmatter grammar: YAML & TOML,
// empty, multi-line, trailing whitespace on fences, CRLF, bodies after, and the
// negative cases (unterminated, indented, four markers, not-at-start) that must
// fall back to ordinary CommonMark identically on both sides.
const CORPUS = [
  '---\ntitle: Hi\n---\n',
  '---\ntitle: Hi\nlist:\n  - a\n  - b\n---\n# Body\n',
  '---\n---\n',
  '---\n\n---\n',
  '---\nonly: value\n---',          // no trailing newline
  '+++\na = 1\n+++\n',
  '+++\n+++\n',
  '+++\n[server]\nport = 80\n+++\nText\n',
  '--- \nyaml: x\n--- \n',           // trailing space on both fences
  '---\t\nyaml: x\n---\t\n',         // trailing tab
  '---\r\ntitle: Hi\r\nx: 1\r\n---\r\n', // CRLF (value keeps internal \r\n verbatim)
  '---\nkey: "a: b"\n---\nbody\n',
  '---\nunterminated\nnot frontmatter\n',  // no close → not frontmatter
  '----\nx\n----\n',                 // four markers → not a fence
  '  ---\nindented\n---\n',          // indented opener → not frontmatter
  '# Heading\n\n---\nx\n---\n',      // not at start → not frontmatter
  'text before\n---\nx\n---\n',      // not at start
  '---\nmultiple\nlines\nhere\n---\nand a body paragraph\n',
  '+++\nmismatched closer\n---\n',   // toml open, yaml close → no close found
  '---\nmismatched closer\n+++\n',   // yaml open, toml close → no close found
];

const ref = unified().use(remarkParse).use(remarkFrontmatter, ['yaml', 'toml']);
const ours = (md) => parseToMdastWire(md, FRONTMATTER);
const theirs = (md) => ref.parse(md);

console.log('frontmatter alignment gate — sparkdown (native) vs remark-frontmatter\n');
const g1 = runGate({
  label: 'Gate 1 — structural (ignore position)',
  items: CORPUS,
  a: ours,
  b: theirs,
  eq: eqMdast({ dropPos: true }),
});
const g2 = runGate({
  label: 'Gate 2 — with position (byte-for-byte)',
  items: CORPUS,
  a: ours,
  b: theirs,
  eq: eqMdast({ dropPos: false }),
});

process.exit(g1.ok && g2.ok ? 0 : 1);
