# SPIKE: sparkdown as a unified-compatible parser (the consumer shape)

**Question:** can a consumer app (Next.js, Astro, an MDX pipeline) use sparkdown
the way they already use remark — `unified().use(parser).use(...plugins)` — by
just swapping the parser?

**Method:** wrote the JS glue (`harness/sparkdown.mjs`) that installs sparkdown's
wasm as a unified parser plugin (sets `this.parser`, exactly like `remark-parse`
sets it). Then used it verbatim in the genuine unified flow and compared to the
real `remark-parse` pipeline. See `harness/usage_demo.mjs`.

## The consumer API (real, unmodified)

```js
import { unified } from 'unified';
import sparkdown from '@momiji-rs/sparkdown/unified'; // the glue module
import remarkEmoji from 'remark-emoji';
import remarkToc from 'remark-toc';
import remarkRehype from 'remark-rehype';
import rehypeSlug from 'rehype-slug';
import rehypeStringify from 'rehype-stringify';

const html = String(await unified()
  .use(sparkdown)        // ← parse markdown → mdast IN WASM (drop-in for remark-parse)
  .use(remarkEmoji)
  .use(remarkToc)
  .use(remarkRehype)
  .use(rehypeSlug)
  .use(rehypeStringify)
  .process(markdown));
```

That is the standard remark/rehype usage with **one line changed** (`remarkParse`
→ `sparkdown`).

## Result

- The pipeline produces correct output (TOC injected, `:tada:`→🎉, heading slugs,
  links). ✅
- **Identical to the same pipeline on `remark-parse`** on the demo doc, and
  **650 / 652 (99.7%)** across the CommonMark corpus. The 2 diffs are the known
  edge cases (a `<style>` HTML block #173; nested-image alt #574).

## Reading

- **It's a genuine drop-in.** `.process(md)` runs *our* parser, then the user's
  unmodified mdast/hast plugins, then the compiler — the real unified machinery,
  not a reimplementation.
- The glue is tiny (~40 lines): instantiate the wasm once, set `this.parser`. This
  is the shape of the npm package's `/unified` entry point.
- Synchronous parse (unified requires it): Node compiles wasm synchronously at
  import; a browser build would top-level-await an async instantiate.

## What a real package still needs (engineering, not feasibility)

1. **parser-extension plugins won't apply** (remark-gfm/frontmatter/directive/math
   change *parsing*). Those features come from sparkdown core (GFM exists as a
   Cargo feature; frontmatter/math/directive/MDX would be core work). Transform
   plugins — the majority — work today.
2. **position fidelity** (indent-aware column/offset, per-inline offsets) for
   strict source-mapping plugins.
3. packaging: ship the `.wasm` + this glue as a dual ESM/CJS entry, browser +
   Node builds, types.

## Verdict

The consumer story holds: **swap one line and the entire transform-plugin half of
the remark/rehype ecosystem works on sparkdown — identically to remark, faster.**
All five spikes together (AST ~1.4×, mdast 99.8% render-parity, boundary 13–23×,
real plugins 99.7%, position read by remark-lint) prove the "Rust core + JS
plugins" product end-to-end. Nothing left is a feasibility risk — only build-out.

Run it: `node harness/usage_demo.mjs`.
