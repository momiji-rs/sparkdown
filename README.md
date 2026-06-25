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

Where [rostdown](https://github.com/momiji-rs/rostdown) renders a kramdown
**subset** and cleanly declines everything outside it, sparkdown's contract is
the inverse: parse the **whole** [CommonMark 0.31.2](https://spec.commonmark.org/0.31.2/)
grammar and aim for byte-identical output with the reference `cmark`. It's
built first for ourselves — a fast, dependency-free CommonMark engine that's
pleasant to embed — not to chase any particular competitor.

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
whitespace byte-for-byte.

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

| engine                 |     time | relative |
| ---------------------- | -------: | -------: |
| **sparkdown**          | **0.58 ms** | **1.00×** |
| pulldown-cmark         |  0.67 ms |    1.16× |
| rushdown (cached)      |  1.36 ms |    2.35× |
| cmark (C, reference)   |  ~1.9 ms |   ~3.2×  |
| comrak                 |  2.00 ms |    3.45× |
| markdown-rs            |  33.5 ms |      58× |

`cargo bench` reproduces the sparkdown-vs-pulldown pair in-repo. Ratios are the
portable part — absolute times are machine-specific. cmark was compared
CLI-to-CLI on a 16 MB document (3.2× sparkdown's wall time and 3.1× its retired
instructions); goldmark (Go) benches in comrak's tier.

**As WebAssembly** — the npm package [`@momiji-rs/sparkdown`](#webassembly-npm)
renders the same spec in **~1.33 ms** (`+simd128`, `+bulk-memory`; 652/652
through the wasm), *still ahead of rushdown's native build*. The wasm tax over
the 0.58 ms native is mostly the VM/bounds-check/`dlmalloc` floor — the lost
SIMD and `memory.copy` are recovered by the wasm `v128` kernels and bulk-memory.

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

## WebAssembly (npm)

A **WASI-free** WebAssembly build ships on npm as
[`@momiji-rs/sparkdown`](https://www.npmjs.com/package/@momiji-rs/sparkdown) —
zero dependencies, self-contained (the wasm is base64-inlined), and runs in
Node, browsers, bundlers, Deno, Bun, and edge runtimes with no `fetch`/`fs`/WASI
setup.

```bash
npm install @momiji-rs/sparkdown
```

```js
import { toHtml } from "@momiji-rs/sparkdown";
const html = await toHtml("# Hello *world*");
```

Under the hood it's a `wasm32-unknown-unknown` build behind a tiny raw C-ABI
(`sparkdown_alloc` / `sparkdown_free` / `sparkdown_to_html`) — no `wasm-bindgen`
— so the same `.wasm` drives from any host. Enable the Rust side with the
`wasm` feature:

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
