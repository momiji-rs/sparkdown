# @momiji-rs/sparkdown

Fast, standards-first **CommonMark → HTML** — and the fastest drop-in
**`remark-parse`** — compiled to **WASI-free WebAssembly**.

- ✅ **100% CommonMark 0.31.2** — all 652 official examples pass *through the wasm*.
- 🚀 **~0.9 ms** to render the 200 KB CommonMark spec — ~5× faster than
  markdown-it/marked, and faster than rushdown's *native* build; the native
  crate does it in ~0.59 ms.
- 🌳 **Fastest `remark-parse`.** The [`/mdast`](#mdast--the-fastest-remark-parse)
  subpath emits a unist/mdast tree that **deep-equals `mdast-util-from-markdown`**
  on all 652 examples — ~80× faster, drop-in for unified / remark / rehype.
- 🪶 **Zero dependencies, self-contained.** The wasm is base64-inlined, so it
  works in Node, browsers, bundlers, Deno, Bun, and edge runtimes (Cloudflare
  Workers, Fastly, Vercel) with no `fetch`, `fs`, WASI, or asset configuration.

```bash
npm install @momiji-rs/sparkdown
```

```js
import { toHtml } from "@momiji-rs/sparkdown";

const html = await toHtml("# Hello *world*\n\nWith a [link](https://example.com).");
// <h1>Hello <em>world</em></h1>\n<p>With a <a href="https://example.com">link</a>.</p>\n
```

Importing has **no side effect** — the wasm instantiates lazily. For synchronous
use, call `initSync()` once (server-side: Node/Bun/Deno/edge/workers, *not* the
browser main thread) or `await ready` in browsers:

```js
import { initSync, toHtmlSync } from "@momiji-rs/sparkdown";

initSync(); // sync, idempotent — once, on first use
for (const post of posts) post.html = toHtmlSync(post.markdown); // sync, hot loop
```

## API

| export | signature | notes |
| --- | --- | --- |
| `toHtml` | `(markdown: string) => Promise<string>` | lazily instantiates the wasm |
| `toHtmlSync` | `(markdown: string) => string` | needs a prior `initSync()` / `await ready` |
| `initSync` | `() => unknown` | **sync** instantiation (idempotent); server-side only |
| `ready` | `PromiseLike<void>` | lazily instantiates once awaited |
| `init` | `() => Promise<unknown>` | async instantiation (idempotent) |

`toHtml` is also the default export. TypeScript types are bundled.

## Extensions — the `/gfm` and `/full` subpaths

The package root is pure CommonMark (smallest wasm). Two subpaths add extensions
— same engine, a larger wasm, and a `toHtml(md, options)` that toggles each one
per call:

```js
import { toHtml } from "@momiji-rs/sparkdown/gfm";  // + GitHub Flavored Markdown
await toHtml("~~done~~ and www.example.com");
await toHtml(md, { tables: false }); // toggle a flag

import { toHtml } from "@momiji-rs/sparkdown/full"; // + every extension
await toHtml(":::note\n`:tada:` ~~x~~\n:::");       // directives, emoji, … on by default
```

| subpath | adds | wasm |
| --- | --- | --- |
| `@momiji-rs/sparkdown` | — (CommonMark) | ~182 KB |
| `@momiji-rs/sparkdown/gfm` | strikethrough, tasklists, autolinks, tag filter, tables, footnotes | ~239 KB |
| `@momiji-rs/sparkdown/full` | + frontmatter, emoji, definition lists, directives, diagrams, external-link `rel`, heading ids | ~319 KB |
| `@momiji-rs/sparkdown/mdast` | markdown → mdast tree + a unified `remark-parse` plugin (see below) | ~440 KB |

A bundler ships only the subpath you import. The `.` / `/gfm` / `/full` entries
share the same `toHtml` / `toHtmlSync` / `initSync` / `ready` / `init` surface;
`/mdast` adds `toMdast` and a unified parser plugin.

## mdast — the fastest `remark-parse`

`@momiji-rs/sparkdown/mdast` emits a full **mdast (unist)** tree — the exact shape
`remark-parse` produces — as a drop-in, **~80× faster** parser for the unified /
remark / rehype ecosystem:

```js
import { unified } from "unified";
import sparkdownParse from "@momiji-rs/sparkdown/mdast"; // drop-in remark-parse
import remarkRehype from "remark-rehype";
import rehypeStringify from "rehype-stringify";

await sparkdownParse.ready; // browsers only; Node/Bun/Deno/edge need no await
const html = String(unified()
  .use(sparkdownParse)
  .use(remarkRehype)
  .use(rehypeStringify)
  .processSync(markdown));
```

Or call it directly — `toMdast(md, options)` → tree, `toHtml(md, options)` → HTML
(an in-wasm `mdast → html` render, byte-identical to `mdast-util-to-hast` +
`hast-util-to-html`). The tree is **plain unist objects** (not opaque handles), so
every remark plugin works unmodified; it **deep-equals `mdast-util-from-markdown`
on all 652** CommonMark examples *including* `position`. Pass `{ position: false }`
for ~2× faster parsing when your plugins don't read source positions.

| export | signature |
| --- | --- |
| `sparkdownParse` (default) | unified parser plugin — `unified().use(sparkdownParse)` |
| `toMdast` / `toMdastSync` | `(markdown, options?) => mdast tree` |
| `toHtml` / `toHtmlSync` | `(markdown, options?) => string` (in-wasm mdast→html) |
| `initSync` / `init` / `ready` | instantiation (as in the other entries) |

## How it's built

The package wraps a `wasm32-unknown-unknown` build of the
[sparkdown](https://github.com/momiji-rs/sparkdown) Rust crate via a tiny raw
C-ABI (`sparkdown_alloc` / `sparkdown_free` / `sparkdown_to_html`) — no
`wasm-bindgen`. The wasm uses `+simd128` (vectorized inline scan + escape) and
`+bulk-memory` (native `memory.copy`), so it needs a host from ~2021 onward
(all current browsers, Node 16+, Deno, Bun, modern edge runtimes).

## License

MIT.
