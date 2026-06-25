# SPIKE: unist `position` + a position-reading plugin

**Question:** position-reading plugins (`remark-lint`, source-mapping) need
`position` on nodes. Can sparkdown emit it, and does a real lint rule read it and
produce correct diagnostics?

**Method:** sparkdown's parser tracks line numbers and line-start offsets. ast.rs
attaches unist `position` to block nodes (from the start line + a line-offset
table) and to inline nodes (block-granular). Then ran real `remark-lint` rules —
`remark-lint-no-multiple-toplevel-headings` and `remark-lint-maximum-heading-length`
— on sparkdown's wasm-produced tree and compared diagnostics to remark's tree.
See `harness/lint_demo.mjs`.

## Result

Lint diagnostics on sparkdown's tree, with `line:column` taken from our positions:

```
5:1-5:53: Unexpected duplicate toplevel heading, expected a single heading with rank `1`
7:1-7:82: Unexpected `78` characters in heading, expected at most `60` characters
```

**Identical to running the same rules on remark's own tree.** ✅

Heading-position accuracy across the 652 examples (exact line+column+offset vs
remark): **56 / 71 (78.9%)**. The 15 misses are all **leading-indented** headings
(ATX/setext with 1–3 spaces, or nested) — **line is always correct**; only
column/offset drift, because we currently assume column 1.

Regression: `valid` 100%, `shape` 82.4%, `rt-ref` 99.8%, real-plugin pipeline
650/652 — all unchanged (position is stripped in shape compare and ignored in
rendering).

## What we learned (a real requirement, not obvious upfront)

Newer `remark-lint` rules require **every** node to carry a `position` — including
inline/text nodes — not just the block being reported. With block-only positions
the rule silently produced no diagnostics; adding inline positions made it fire
correctly. So "emit position" means *all* nodes, not just blocks.

## Accuracy tiers (current)

- **Line numbers:** correct everywhere (blocks). ✅ — enough for the bulk of
  line-based lint rules.
- **Column/offset (blocks):** exact when the block starts at column 1 (the common
  case); off by the indent for leading-indented / nested blocks.
- **Inline nodes:** block-granular (valid + lets plugins run); not per-token
  accurate.

## Remaining work for full position fidelity

1. **Block column/offset under indentation:** use the parser's `next_nonspace` /
   container offset instead of assuming column 1. (We already track it internally.)
2. **Per-inline offsets:** thread source byte-spans through the inline tokenizer
   so each inline node gets an exact range. This is the larger piece — sparkdown
   reassembles block content (stripping list/quote prefixes), so inline→source
   mapping needs spans captured during parsing. The zero-copy design means the
   spans exist; they just aren't surfaced yet.
3. **UTF-16 offsets:** unist offsets are JS string indices (UTF-16 code units);
   ours are byte offsets. Identical for ASCII; needs conversion for non-ASCII.

## Verdict

Position works: a real `remark-lint` rule reads sparkdown's positions and emits
**byte-identical diagnostics** to remark. Line-accurate today; full column/offset
fidelity is a bounded, known engineering task (the parser already has the data).
This closes the last feasibility question raised by the plugin spike.
