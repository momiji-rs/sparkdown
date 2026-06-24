# sparkdown

A fast, **standards-first** CommonMark parser in Rust.

*markdown, with a spark.*

Where [rostdown](https://github.com/momiji-rs/rostdown) renders a kramdown
**subset** and cleanly declines everything outside it, sparkdown's contract is
the inverse: parse the **whole** [CommonMark 0.31.2](https://spec.commonmark.org/0.31.2/)
grammar and aim for byte-identical output with the reference `cmark`. The
speed bar to beat is `pulldown-cmark`.

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
whitespace byte-for-byte. Next: wire it into rostdown's perf substrate (arena +
SWAR scan) and benchmark against `pulldown-cmark`.

```bash
# Live CommonMark conformance number (652 official examples):
cargo test --test spec -- --nocapture

# Speed vs pulldown-cmark on the 198 KB spec fixture:
cargo bench
```

## What was reused from rostdown (and what wasn't)

The performance substrate is grammar-agnostic and lifted **verbatim**:

| Module          | Role                                      |
| --------------- | ----------------------------------------- |
| `src/scan.rs`   | SWAR byte search (zero-dep, no `unsafe`)  |
| `src/bump.rs`   | always-on local bump arena                |
| `src/arena.rs`  | opt-in `ScopedAlloc` global allocator (`arena` feature) |
| `src/entities.rs` | HTML entity tables                      |

The grammar layer (`src/parse.rs`, `src/render.rs`) is **new** — rostdown's
parser is built around "decline if outside my subset", and its renderer bakes
in kramdown semantics (heading auto-ids, default smart typography). Both are
the opposite of what a full CommonMark engine needs, so they were rewritten
rather than forked.

> Stage 2 (later): extract `scan`/`bump`/`arena`/`entities` into a shared
> `momiji-core` crate that both rostdown and sparkdown depend on, so a faster
> NEON/SWAR scan improves both at once.

## Build-out order

1. ~~ATX + setext headings, thematic breaks, indented + fenced code blocks.~~ ✅
2. ~~Inline pass: escapes, breaks, entities, code spans, emphasis, autolinks.~~ ✅
3. ~~Links & images (inline + reference) — the other half of the inline pass.~~ ✅
4. ~~Block quotes and list items (the container-block state machine).~~ ✅
5. ~~HTML blocks (and raw inline HTML), link reference definitions.~~ ✅

**100% conformance reached.** Remaining work is performance: share rostdown's
`core` (arena + SWAR scan), then close the speed gap to `pulldown-cmark`.

## License

MIT. The vendored CommonMark spec suite under `tests/fixtures/spec.json` is
MIT, © John MacFarlane and contributors.
