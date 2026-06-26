// Perf verification (task #12): native external-link transform vs the
// rehype-external-links JS pipeline (the ecosystem equivalent). Run:
//   node perf_external_links.mjs

import { unified } from 'unified';
import remarkParse from 'remark-parse';
import remarkRehype from 'remark-rehype';
import rehypeExternalLinks from 'rehype-external-links';
import rehypeStringify from 'rehype-stringify';
import { best, report, sparkOpts } from './perf_harness.mjs';

const EXTERNAL = 2048;

// A link-heavy document: 200 paragraphs each with an external and a local link.
let doc = '';
for (let i = 0; i < 200; i++) {
  doc += `See [site ${i}](https://example.com/page/${i}) and [local ${i}](/docs/${i}).\n\n`;
}

const ref = unified()
  .use(remarkParse)
  .use(remarkRehype)
  .use(rehypeExternalLinks, {})
  .use(rehypeStringify);

report('link-heavy doc → HTML (200 ext + 200 local), best-of-15', [
  { name: 'sparkdown built-in (all wasm)', ms: best(() => sparkOpts(doc, EXTERNAL), { iters: 50 }) },
  { name: 'rehype-external-links pipeline (JS)', ms: best(() => String(ref.processSync(doc)), { iters: 20 }) },
]);
