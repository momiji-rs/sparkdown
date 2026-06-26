// Perf verification (task #10): native directives grammar vs the remark-directive
// JS pipeline (the ecosystem equivalent). remark-directive has no built-in HTML,
// so the reference pipeline uses the handler from its README (name → element,
// attributes → properties). Run: node perf_directives.mjs

import { unified } from 'unified';
import remarkParse from 'remark-parse';
import remarkDirective from 'remark-directive';
import remarkRehype from 'remark-rehype';
import rehypeStringify from 'rehype-stringify';
import { visit } from 'unist-util-visit';
import { h } from 'hastscript';
import { best, report, sparkOpts } from './perf_harness.mjs';

const DIRECTIVES = 8192;

// The remark-directive README's HTML handler.
function directiveToHast() {
  return (tree) => {
    visit(tree, (node) => {
      if (
        node.type === 'containerDirective' ||
        node.type === 'leafDirective' ||
        node.type === 'textDirective'
      ) {
        const data = node.data || (node.data = {});
        const hast = h(node.name, node.attributes || {});
        data.hName = hast.tagName;
        data.hProperties = hast.properties;
      }
    });
  };
}

// A doc with a mix of all three directive forms.
let doc = '';
for (let i = 0; i < 150; i++) {
  doc += `:::note{#n${i} .box}\nText with a :hl[span ${i}]{.k} and more.\n:::\n\n::break\n\n`;
}

const ref = unified()
  .use(remarkParse)
  .use(remarkDirective)
  .use(directiveToHast)
  .use(remarkRehype)
  .use(rehypeStringify);

report('directive-heavy doc → HTML (150 containers + text + leaf), best-of', [
  { name: 'sparkdown built-in (all wasm)', ms: best(() => sparkOpts(doc, DIRECTIVES), { iters: 50 }) },
  { name: 'remark-directive pipeline (JS)', ms: best(() => String(ref.processSync(doc)), { iters: 20 }) },
]);
