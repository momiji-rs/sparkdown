# @momiji-rs/sparkdown

Fast, standards-first **CommonMark → HTML**, compiled to **WASI-free WebAssembly**.

- ✅ **100% CommonMark 0.31.2** — all 652 official examples pass *through the wasm*.
- 🚀 **~0.9 ms** to render the 200 KB CommonMark spec — ~5× faster than
  markdown-it/marked, and faster than rushdown's *native* build; the native
  crate does it in ~0.59 ms.
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

## How it's built

The package wraps a `wasm32-unknown-unknown` build of the
[sparkdown](https://github.com/momiji-rs/sparkdown) Rust crate via a tiny raw
C-ABI (`sparkdown_alloc` / `sparkdown_free` / `sparkdown_to_html`) — no
`wasm-bindgen`. The wasm uses `+simd128` (vectorized inline scan + escape) and
`+bulk-memory` (native `memory.copy`), so it needs a host from ~2021 onward
(all current browsers, Node 16+, Deno, Bun, modern edge runtimes).

## License

MIT.
