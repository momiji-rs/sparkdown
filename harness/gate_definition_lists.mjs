// Alignment gate (task #11): sparkdown's native definition-list grammar vs
// remark-definition-list (the pandoc-style `Term\n: def` extension). Run:
//   node gate_definition_lists.mjs

import { unified } from 'unified';
import remarkParse from 'remark-parse';
import remarkRehype from 'remark-rehype';
import rehypeStringify from 'rehype-stringify';
import remarkDefinitionList, { defListHastHandlers } from 'remark-definition-list';
import { runGate, eqJson } from './gate_harness.mjs';
import { sparkOpts } from './perf_harness.mjs';

const DEFLIST = 4096;

// Representative cases — tight/loose, single/multi term, multi-def, merging,
// inline markup, and the non-deflist negatives (orphan marker, no space).
const CORPUS = [
  'Term\n: Definition\n',
  'Apple\n: Pomaceous fruit\n: Computer company\n',
  'T1\nT2\n: D\n',
  'Term\n\n: Loose def\n',
  'T1\n: D1\n\nT2\n: D2\n',
  'Term\n: First\n\n: Second\n',
  'before\n\nTerm\n: Def\n\nafter\n',
  'Term\n: First\n: Second\n',
  ': orphan no term\n',
  'Term\n:NoSpace\n',
  'Term\n:  two spaces\n',
  '*Rich* term\n: def with *em*\n',
  'Term\n: a\nb\n',
  '# Heading\n\nTerm\n: Def\n',
];

const ref = unified()
  .use(remarkParse)
  .use(remarkDefinitionList)
  .use(remarkRehype, { handlers: defListHastHandlers })
  .use(rehypeStringify);

console.log('definition-list gate — sparkdown (native) vs remark-definition-list\n');
const g = runGate({
  label: 'HTML vs remark-definition-list (trailing-trimmed)',
  items: CORPUS,
  a: (md) => sparkOpts(md, DEFLIST).trimEnd(),
  b: (md) => String(ref.processSync(md)).trimEnd(),
  eq: eqJson,
  limit: 14,
});

process.exit(g.ok ? 0 : 1);
