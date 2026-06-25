# sparkdown

[![CI](https://github.com/momiji-rs/sparkdown/actions/workflows/ci.yml/badge.svg)](https://github.com/momiji-rs/sparkdown/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/sparkdown.svg)](https://crates.io/crates/sparkdown)
[![npm](https://img.shields.io/npm/v/@momiji-rs/sparkdown.svg)](https://www.npmjs.com/package/@momiji-rs/sparkdown)
[![docs.rs](https://img.shields.io/docsrs/sparkdown)](https://docs.rs/sparkdown)
[![CommonMark 0.31.2](https://img.shields.io/badge/CommonMark-0.31.2%20100%25-brightgreen.svg)](https://spec.commonmark.org/0.31.2/)
[![MSRV](https://img.shields.io/badge/MSRV-1.95%2B-blue.svg)](Cargo.toml)
[![dependencies](https://img.shields.io/badge/dependencies-0-brightgreen.svg)](Cargo.toml)
[![license](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

A fast, **standards-first** CommonMark parser in Rust.

*markdown, with a spark.*

sparkdown parses the **whole** [CommonMark 0.31.2](https://spec.commonmark.org/0.31.2/)
grammar — all 652 conformance examples — and matches the reference `cmark`
byte-for-byte. It's a **zero-dependency** engine built to be pleasant to embed,
with a default build tuned to be the fastest CommonMark parser we know of and
[GitHub Flavored Markdown](#gfm-extensions-opt-in) available as opt-in extensions
the default build doesn't even compile. The speed is a means — a clean, embeddable
engine — not a scoreboard.

## Status — ✅ 100% CommonMark 0.31.2

Passes all **652/652** examples of the official conformance suite.

- **Blocks:** paragraphs, ATX + setext headings, thematic breaks, fenced +
  indented code blocks, block quotes, bullet + ordered lists (tight/loose,
  nested), HTML blocks (all 7 conditions), link reference definitions.
- **Inlines:** backslash escapes, hard/soft line breaks, the full HTML5 entity
  set + numeric references, code spans, emphasis + strong emphasis
  (delimiter-stack algorithm), links & images (inline + full/collapsed/shortcut
  reference), autolinks, raw inline HTML.

The block layer is a faithful port of the reference incremental algorithm
(open-block tree + per-line continuation); the renderer matches cmark's
whitespace byte-for-byte. [GitHub Flavored Markdown](#gfm-extensions-opt-in) is
available as an opt-in feature on top.

```bash
# Live CommonMark conformance number (652 official examples):
cargo test --test spec -- --nocapture

# Throughput on the 200 KB CommonMark spec fixture (vs pulldown-cmark):
cargo bench
```

## Performance

On the CommonMark spec itself (`tests/fixtures/data.md`, ~200 KB — the same
fixture `cmark` and rushdown benchmark on), default build, Apple Mac Studio,
measured in process:

| engine                       |        time | relative |
| ---------------------------- | ----------: | -------: |
| **sparkdown**                | **0.59 ms** |   1.00×  |
| **sparkdown — GFM, all on** ¹ | **0.63 ms** |    1.07× |
| pulldown-cmark               |     0.68 ms |    1.16× |
| **sparkdown (wasm)** ²       |     0.93 ms |    1.58× |
| **sparkdown (wasm, GFM all)** ² | 0.98 ms |    1.66× |
| rushdown (cached)            |     1.36 ms |    2.31× |
| cmark (C, reference)         |     ~1.9 ms |   ~3.2×  |
| comrak                       |     2.00 ms |    3.45× |
| markdown-rs                  |     33.5 ms |      58× |

¹ The opt-in [`gfm`](#gfm-extensions-opt-in) build with **every** extension
active (strikethrough, task lists, extended autolinks, tag filter, tables) —
still faster than pulldown, which has none of them on.

² WebAssembly (`+simd128 +bulk-memory`, warm reusable context), measured in Node;
every non-wasm row is a native build. Both wasm builds — pure and full-GFM —
still beat rushdown's *native* build.

`cargo bench` reproduces the sparkdown-vs-pulldown pair in-repo. Ratios are the
portable part — absolute times are machine-specific. cmark was compared
CLI-to-CLI on a 16 MB document (3.2× sparkdown's wall time and 3.1× its retired
instructions); goldmark (Go) benches in comrak's tier.

The wasm row's tax over the 0.59 ms native is mostly the VM / bounds-check /
`dlmalloc` floor: the *lost SIMD* and *byte-loop memcpy* are recovered by the
wasm `v128` kernels and `+bulk-memory` (`memory.copy`), and a **warm reusable
context** (buffers held across renders) keeps it to ~1.6×. See
[WebAssembly](#webassembly).

The speed is from the default build alone — zero dependencies, no feature flags:

- **Zero-copy** source borrowing for paragraph / code / HTML-block text, link
  destinations and titles, and reference-lookup keys.
- **SIMD** byte-set matchers (the simdjson nibble-lookup, NEON / SSE) skip plain
  text to the next significant byte, and a fused SIMD `escape_html` emits a whole
  16-byte block per compare.
- **Lean structures** — an intrusive first-child / next-sibling node tree, a
  reused inline scratch buffer, and on-the-fly line iteration (no materialized
  line vector).

<sub>rushdown's published benchmark lists pulldown-cmark at ~6 ms; that figure is
inflated by a copy-paste bug in its harness — the pulldown timing closure also
runs comrak. Measured correctly on the same fixture, pulldown-cmark is ~0.67 ms.</sub>

## GFM extensions (opt-in)

[GitHub Flavored Markdown](https://github.github.com/gfm/) is a **compile-time
opt-in**. The default build contains *no* GFM code — the parser stays the
byte-for-byte pure-CommonMark fast path above. Enable the `gfm` Cargo feature to
unlock the runtime `Options` API:

```toml
[dependencies]
sparkdown = { version = "0", features = ["gfm"] }
```

```rust
use sparkdown::{to_html_with, Options};

// Options::gfm() turns on every extension; or set individual flags.
let html = to_html_with("~~done~~ and www.example.com", &Options::gfm());
```

| flag            | extension                                   |
| --------------- | ------------------------------------------- |
| `strikethrough` | `~~text~~` → `<del>`                         |
| `tasklist`      | `- [ ]` / `- [x]` checkbox list items       |
| `autolink`      | bare `www.` / `http(s)://` / email links    |
| `tagfilter`     | neutralize unsafe raw-HTML tags (`<script>…`) |
| `tables`        | pipe tables with column alignment           |
| `hard_wraps`    | every soft line break → `<br />`            |

Each flag is gated at a block- or inline-*type* boundary, and profiled so that
even with **all** extensions active the parser still beats pulldown-cmark (the
0.63 ms row above). For a warm, reusable context use
`Renderer::with_options(opts)`.

## WebAssembly

A **WASI-free** WebAssembly build runs in Node, browsers, bundlers, Deno, Bun,
and edge runtimes with no `fetch`/`fs`/WASI setup. It ships two ways:

- **npm** — [`@momiji-rs/sparkdown`](https://www.npmjs.com/package/@momiji-rs/sparkdown)
  (pure CommonMark) and `@momiji-rs/sparkdown-gfm` (with GFM). Zero dependencies,
  self-contained (the wasm is base64-inlined). Also on jsDelivr/unpkg for free.
- **GitHub Releases** — the raw `sparkdown.wasm` / `sparkdown-gfm.wasm` modules
  (with SHA-256 sums) are attached to each tagged release, for non-JS hosts
  (wasmtime, wazero, Workers, Python, …) that want the bare module.

Against the popular JS markdown libraries, in Node on the same 198 KB document,
sparkdown's **wasm** still wins comfortably:

| library          |    time | relative |
| ---------------- | ------: | -------: |
| **sparkdown (wasm)** | **0.90 ms** | 1.00× |
| markdown-it      | 4.52 ms |    5.0× |
| marked           | 5.20 ms |    5.7× |

```bash
npm install @momiji-rs/sparkdown
```

```js
import { toHtml } from "@momiji-rs/sparkdown";
const html = await toHtml("# Hello *world*");
```

Importing the package has **no side effect** — the wasm instantiates lazily on
first use. For synchronous, server-side rendering (Node / Bun / Deno / edge /
workers — *not* the browser main thread, where sync wasm compile is capped at
~4 KB), call `initSync()` once and then `toHtmlSync`:

```js
import { initSync, toHtmlSync } from "@momiji-rs/sparkdown";
let ready = false;
function render(md) {
  if (!ready) { initSync(); ready = true; } // sync, once, on first render
  return toHtmlSync(md);
}
```

In the browser, keep using `await toHtml(...)` (or `await ready` then `toHtmlSync`).

Under the hood it's a `wasm32-unknown-unknown` build behind a tiny raw C-ABI
(`sparkdown_alloc` / `sparkdown_free` / `sparkdown_to_html`, plus
`sparkdown_to_html_opts(ptr, len, flags)` in the GFM build) — no `wasm-bindgen`,
so the same `.wasm` drives from any host. Build it yourself with the `wasm`
feature (add `,gfm` for the GFM build):

```bash
RUSTFLAGS="-C target-feature=+simd128,+bulk-memory" \
  cargo build --release --features wasm --target wasm32-unknown-unknown
```

## What was reused from rostdown (and what wasn't)

The performance substrate is grammar-agnostic and lifted **verbatim**:

| Module          | Role                                      |
| --------------- | ----------------------------------------- |
| `src/scan.rs`   | SWAR byte search (zero-dep, no `unsafe`)  |
| `src/bump.rs`   | always-on local bump arena                |
| `src/arena.rs`  | opt-in `ScopedAlloc` global allocator (`arena` feature) |
| `src/entities.rs` | HTML entity tables                      |

The grammar layer (`src/block.rs`, `src/inline.rs`, `src/render.rs`) is
**new** — rostdown's
parser is built around "decline if outside my subset", and its renderer bakes
in kramdown semantics (heading auto-ids, default smart typography). Both are
the opposite of what a full CommonMark engine needs, so they were rewritten
rather than forked.

> Stage 2 (later): extract `scan`/`bump`/`arena`/`entities` into a shared
> `momiji-core` crate that both rostdown and sparkdown depend on, so a faster
> NEON/SWAR scan improves both at once.

## License

MIT. The vendored CommonMark spec suite under `tests/fixtures/spec.json` is
MIT, © John MacFarlane and contributors.
