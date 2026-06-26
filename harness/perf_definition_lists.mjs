// Perf verification (task #11): native definition-list grammar vs the
// remark-definition-list JS pipeline (the ecosystem equivalent). Run:
//   node perf_definition_lists.mjs

import { unified } from 'unified';
import remarkParse from 'remark-parse';
import remarkRehype from 'remark-rehype';
import rehypeStringify from 'rehype-stringify';
import remarkDefinitionList, { defListHastHandlers } from 'remark-definition-list';
import { best, report, sparkOpts } from './perf_harness.mjs';

const DEFLIST = 4096;

// A glossary-style document: 200 term/definition pairs.
let doc = '';
for (let i = 0; i < 200; i++) {
  doc += `Term ${i}\n: Definition number ${i} with some *emphasis* and text.\n\n`;
}

const ref = unified()
  .use(remarkParse)
  .use(remarkDefinitionList)
  .use(remarkRehype, { handlers: defListHastHandlers })
  .use(rehypeStringify);

report('glossary doc → HTML (200 term/def pairs), best-of', [
  { name: 'sparkdown built-in (all wasm)', ms: best(() => sparkOpts(doc, DEFLIST), { iters: 50 }) },
  { name: 'remark-definition-list pipeline (JS)', ms: best(() => String(ref.processSync(doc)), { iters: 20 }) },
]);
