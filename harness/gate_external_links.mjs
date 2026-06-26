// Alignment gate (task #12): sparkdown's native external-link transform vs
// rehype-external-links (default config). Run: node gate_external_links.mjs

import { unified } from 'unified';
import remarkParse from 'remark-parse';
import remarkGfm from 'remark-gfm';
import remarkRehype from 'remark-rehype';
import rehypeExternalLinks from 'rehype-external-links';
import rehypeStringify from 'rehype-stringify';
import { runGate, eqJson } from './gate_harness.mjs';
import { sparkOpts } from './perf_harness.mjs';

const AUTOLINK = 4;
const EXTERNAL = 2048;
const FLAGS = AUTOLINK | EXTERNAL;

const CORPUS = [
  '[x](https://a.com)\n',
  '[x](http://a.com)\n',
  '[x](http://a.com "title")\n',
  '[rel](/path) and [frag](#sec)\n',
  '[mail](mailto:a@b.com)\n',
  '[up](HTTPS://X) stays plain\n', // case-sensitive: uppercase not external
  '[ftp](ftp://h/f) stays plain\n',
  'autolink https://auto.org/p here\n', // GFM www/url autolink
  'bare www.example.com link\n',
  '<https://angle.org> autolink\n',
  'mixed [a](https://x) and [b](/y) and [c](http://z)\n',
  'two https://one.com and https://two.com\n',
];

const ref = unified()
  .use(remarkParse)
  .use(remarkGfm)
  .use(remarkRehype)
  .use(rehypeExternalLinks, {})
  .use(rehypeStringify);

console.log('external-links gate — sparkdown (native) vs rehype-external-links\n');
const g = runGate({
  label: 'HTML vs rehype-external-links (trailing-trimmed)',
  items: CORPUS,
  a: (md) => sparkOpts(md, FLAGS).trimEnd(),
  b: (md) => String(ref.processSync(md)).trimEnd(),
  eq: eqJson,
  limit: 12,
});

process.exit(g.ok ? 0 : 1);
