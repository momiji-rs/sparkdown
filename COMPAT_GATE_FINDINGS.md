# SPIKE: the compatibility gate (the "100% compatible" metric)

**Question:** what single, falsifiable metric expresses "100% compatible with
mdast/remark", and how far are we / what's the exact path to 100%?

**The metric.** Our mdast **deep-equals `mdast-util-from-markdown`'s mdast**, over
a corpus. If the trees are identical, *every* downstream consumer (plugin,
renderer, tool) behaves identically — there is no observable difference. This one
number subsumes valid / render-parity / plugin-parity. Two tiers:

- **Gate 1 (product):** deep-equal **ignoring `position`** — drop-in for output
  and the vast majority of plugins.
- **Gate 2 (gold standard):** deep-equal **including `position`** — truly
  indistinguishable (needed by source-mapping / position-precise plugins).

Run: `node harness/gate.mjs` (exits non-zero unless Gate 1 = 100%; CI-ready).

## Current (652 CommonMark examples)

```
Gate 1  deep-equal IGNORING position : 652/652  (100.0%)  ← was 83.0%   ✅
Gate 2  deep-equal INCLUDING position: 652/652  (100.0%)  ← was 19.9%   ✅
```

**Both gates are 100%.** `node harness/gate.mjs` exits 0. Our mdast is now
byte-for-byte **position-identical** to `mdast-util-from-markdown` across the
whole CommonMark suite — every `position` (line/column/offset, UTF-16) matches.
Independently corroborated: `verify.mjs` shape 652/652, the unified drop-in
(`usage_demo.mjs`) and the real-plugin pipeline (`plugin_demo.mjs`) both produce
**byte-identical HTML to remark-parse on all 652**. CommonMark conformance stays
652/652; default/gfm/ast builds all green; everything is behind the `ast` Cargo
feature (default build byte-identical).

## What got fixed (this session): reference-model + spread

Both designed buckets are now **done**; the default build stays byte-identical
(everything is behind the `ast` Cargo feature) and CommonMark **652/652** still
passes.

**reference-model (−76 of 77).** The block parser now emits `Kind::Definition`
nodes (spliced into the child list in source order, ahead of the paragraph they
were stripped from; they render to nothing so HTML is unchanged), and the inline
tokenizer emits `linkReference`/`imageReference` instead of inline-resolving refs
— carrying `identifier` (normalized), `label` (decoded raw), and `referenceType`
(`shortcut`/`collapsed`/`full`), matching `mdast-util-from-markdown` exactly. A
real bug surfaced and was fixed: a failed setext-underline attempt over pure
ref-defs was emitting each `definition` twice (once at the underline attempt,
again at paragraph finalize).

**spread (−33 of 33).** mdast splits CommonMark's single looseness bit into two:
`list.spread` (a blank line *between items*) and `listItem.spread` (a blank line
*between an item's own block children*). `compute_spread` derives both from the
same `ends_with_blank_line` machinery `compute_tight` already uses — and their
disjunction provably equals `!tight`, so HTML looseness is untouched.

## The final 3 (now fixed)

| ex | what it was | fix |
| --- | --- | --- |
| #173 | HTML-block trailing newline. mdast keeps the final `\n` for a type-1 block (`<script>`/`<style>`/`<pre>`) ended by **EOF**, drops exactly one otherwise; `html_trim_end` strips all trailing blanks for rendering. | Track *how* the block closed (`html_closed_by_cond`) and compute a separate mdast end (`html_ast_cend`); render path unchanged. |
| #574 | Nested image alt `![foo ![bar](/url)](/url2)` → `foo ` (lost `bar`): in AST mode the inner image is a `Sem` node the HTML renderer drops, so the alt-builder saw nothing. | `ast_image_alt` — a plain-text fold over the slots mirroring `list_to_tokens` (text + `Sem` leaf values incl. nested alts). |
| #541 | Multi-line label: mdast's `label` keeps the raw inner indentation (`Foo\n  bar`); the paragraph buffer de-indents continuation lines (`Foo\nbar`). | Track each node's raw source span (`src_start`/`src_end`, survives materialization); for top-level paragraphs re-parse labels from source (`recover_raw_labels`). |

(Earlier this session the gate also drove out inlineCode line-ending
normalization — mdast keeps raw line endings — clearing #335/#337/#640/#641.)

## Gate 2 (position) — also closed to 100%

Started at 19.9%. Three foundations got it most of the way: indent-aware block
column/offset (the parser tracks `next_nonspace`), per-inline source spans
threaded through the inline tokenizer (`cspan` per slot), and UTF-16
offset/column conversion (unist offsets are JS string indices, not bytes). The
long tail was a sequence of small, exact source-mapping rules, each landed as an
atomic commit with the gate measured after every step:

- **emphasis/strong marker spans** — the open/close `Tag` spans consume exactly
  `use_delims` markers off the current run bounds (fixed an underflow + nesting).
- **block edges** — blockquote spans to its last `>` line; atx/setext span the
  whole line; thematic-break/setext/fenced-close ends tracked.
- **trailing whitespace** — the final text node drops trailing spaces (value
  *and* position); a soft break's trailing spaces don't leak into the next text
  node's start; indented-code keeps trailing spaces but drops empty lines
  (`rtrim_code_end`); a definition extends over trailing spaces to its line end.
- **HtmlBlock end** follows its `value` (type-1-at-EOF keeps the newline), mapped
  through `content_to_src` for buffered (blockquote/list) blocks.
- **tabs** — indented code after a container marker rounds its start past a
  partially-consumed tab byte (a unist point can't sit inside a byte).
- **list/item edges** — an empty item spans its whole marker line; an unclosed
  fenced code drops its trailing newline unless it ends at EOF (fixes
  blockquote-wrapped fences); a list absorbs a blockquote-marker blank line.

## What this answers

"Do we need 652/652?" — **For both gates: yes, and we hit both.** Reference
nodes + spread took Gate 1 from 83.0% → 99.5%; the final 3 razor-edge cases
(#173/#574/#541) closed it to **100.0%**. Gate 2 then went 19.9% → **100.0%**
through the source-mapping rules above. Gate 1 = 100% is the honest
single-number "drop-in compatible with remark" claim; **Gate 2 = 100% is the
gold standard — our trees are indistinguishable from remark's, positions and
all**, exactly what source-mapping / position-precise plugins (remark-lint,
etc.) need.

Caveat on the corpus: the 652 are CommonMark *conformance* cases (edge-heavy).
Before claiming 100% compatibility, widen the gate to remark's own fixtures + a
real-world `.md` corpus — necessary breadth the spec suite doesn't give.

Caveat on intent: matching remark means matching it **bug-for-bug** (plugins may
depend on its quirks). That's compatibility, and it can diverge from "most
correct" — a deliberate choice for an ecosystem product.
