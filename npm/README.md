# @momiji-rs/sparkdown

Fast, standards-first **CommonMark → HTML**, compiled to **WASI-free WebAssembly**.

- ✅ **100% CommonMark 0.31.2** — all 652 official examples pass *through the wasm*.
- 🚀 **~1.3 ms** to render the 200 KB CommonMark spec (faster than rushdown's
  *native* build); the native crate does it in ~0.58 ms.
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

Synchronous use after a one-time `await ready`:

```js
import { ready, toHtmlSync } from "@momiji-rs/sparkdown";

await ready;
for (const post of posts) post.html = toHtmlSync(post.markdown); // sync, hot loop
```

## API

| export | signature | notes |
| --- | --- | --- |
| `toHtml` | `(markdown: string) => Promise<string>` | lazily instantiates the wasm |
| `toHtmlSync` | `(markdown: string) => string` | requires a prior `await ready` |
| `ready` | `Promise<void>` | resolves once the wasm is instantiated |
| `init` | `() => Promise<unknown>` | force instantiation (idempotent) |

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
