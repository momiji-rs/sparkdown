# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and this project adheres
to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Published as **`@momiji-rs/sparkdown`** (npm) and **`sparkdown`** (crates.io).

## [0.1.0] - 2026-06-27

The `/mdast` subpath — introduced in 0.0.6 for CommonMark — now covers the full
**GFM node set** and reaches **byte-for-byte parity with `remark-parse` +
`remark-gfm`, including unist `position`**. This is the headline of 0.1.0: a
drop-in, far faster `remark-parse` for the unified / remark / rehype ecosystem.

### Added

- **GFM mdast nodes:** real `table` / `tableRow` / `tableCell` (with column
  alignment), task-list items carrying `checked` (and the `[x]` / `[ ]` marker
  stripped from the text), and GFM autolink-literals (`www.`, `http(s)://`,
  email) emitted as `link` nodes.
- **Autolink-literal byte parity:** the `/mdast` autolink output now mirrors
  remark's two-pass model exactly — the micromark **tokenizer** pass (links with
  `position`) unioned with the `mdast-util-gfm-autolink-literal` **tree
  transform** (links and split text with `position: undefined`). Verified
  20000/20000 against `remark` including `position`, and HTML 20000/20000 against
  raw `micromark`. The HTML path stays tokenizer-only by design (matching
  cmark-gfm / micromark); only the mdast path runs the transform (matching remark
  mdast). The transform also recurses into definition-list and directive
  containers.
- **Fumadocs validation:** verified as a drop-in `remark-parse` for
  `@fumadocs/local-md`'s `.md` lane — byte-identical hast tree *and*
  `remark-structure` search index through the full transform + rehype chain, via
  a single `this.parser`-override plugin (no fork).

### Fixed

- mdast `position` / whitespace fidelity: strip trailing tabs from paragraph
  text (not just spaces); extend a nested blockquote's `position` through the
  trailing EOF marker; stop indented-code `position` from extending onto trailing
  blank lines.
- Resolved a pre-existing inline crash (`begin > end`) on autolink-adjacent input
  (`first_last@x.com`, `…a@b.https://…`) by adding a back-scan floor to the `@`
  and `:` autolink scanners.
- `gfm_url_href` un-gated so every feature combination compiles.

### Performance

- mdast autolink transform scans each URL candidate once per match.

### Known limitations

Fuzz-discovered edge cases only (≈0 in realistic documents; all 652 CommonMark
conformance examples and the table / tasklist / whitespace fuzzers pass): see the
"Known limitations" section in the README.

## Earlier releases

`0.0.1`–`0.0.6` (see the `v0.0.x` git tags): the zero-dependency, 100%-conformant
CommonMark 0.31.2 → HTML engine as WASI-free WebAssembly; opt-in GFM and
extensions (footnotes, emoji, definition lists, directives, frontmatter); the
`/gfm` and `/full` bundles; and `0.0.6`'s `/mdast` subpath (CommonMark mdast +
unist `position`, deep-equal to `mdast-util-from-markdown` on all 652 examples)
plus wire/encoder performance work.
