# mdast compatibility harness (SPIKE)

Verifies that sparkdown's emitted **mdast** plugs into the unified/remark
ecosystem, on all 652 CommonMark spec examples.

## Run

```bash
# 1. emit sparkdown's mdast (+ our HTML) for every spec example
cargo run --release --features ast --example mdast_json > harness/sparkdown-mdast.json

# 2. install the real ecosystem packages and run the four checks
cd harness && npm install && node verify.mjs
```

## What it checks

| check    | tool                                            | proves |
| -------- | ----------------------------------------------- | ------ |
| valid    | `mdast-util-assert`                             | the tree is legal mdast |
| shape    | deep-equal vs `mdast-util-from-markdown` (no `position`) | same tree as the remark reference parser |
| rt-ref   | `mdast-util-to-hast` → `hast-util-to-html`, ours vs ref | semantically equivalent to the reference tree |
| rt-cmark | same, ours vs cmark's expected HTML             | renders to the spec's exact HTML |
| plugin   | `mdast-util-to-string` + `unist-util-visit`     | traversable by remark-style plugins |

## Latest result (652 examples)

```
valid     652 / 652  100.0%
shape     537 / 652   82.4%   (77 of the 115 diffs are reference-definitions, by design)
rt-ref    651 / 652   99.8%   ← headline: renders identically to remark's own tree
rt-cmark  541 / 652   83.0%
plugin    652 / 652  100.0%
```

See `../HARNESS_FINDINGS.md` for interpretation and the remaining gaps.

## wasm boundary benchmark

Measures parsing in wasm + shipping the mdast to JS, vs parsing in JS
(`mdast-util-from-markdown`). Build the wasm first (uses the rustup toolchain,
which has the wasm std — Homebrew rust does not):

```bash
RUSTFLAGS="-C target-feature=+simd128,+bulk-memory" \
  ~/.rustup/toolchains/stable-*/bin/cargo build --release \
  --target wasm32-unknown-unknown --features ast,wasm
wasm-opt -O4 --enable-simd --enable-bulk-memory \
  target/wasm32-unknown-unknown/release/sparkdown.wasm -o harness/sparkdown.wasm

node harness/wasm_boundary.mjs   # 198 KB doc, full breakdown
node harness/wasm_sweep.mjs      # 1 KB → 198 KB sweep
```

Result: the wasm path is **~13–23× faster** than parsing in JS, across sizes.
See `../WASM_BOUNDARY_FINDINGS.md`.

## real-plugin demo

Runs genuine npm plugins (`remark-emoji`, `remark-toc`, `rehype-slug`) on
sparkdown's wasm-produced tree and compares to remark's own tree:

```bash
node harness/plugin_demo.mjs
```

Result: real plugins run unmodified and produce **identical HTML to remark's tree
in 650/652 (99.7%)** of the corpus. See `../PLUGIN_FINDINGS.md`.

## position-reading lint demo

Runs real `remark-lint` rules (which read `position`) on sparkdown's wasm tree:

```bash
node harness/lint_demo.mjs
```

Result: lint diagnostics (with `line:col`) are **identical to remark's tree**;
heading line numbers correct everywhere, column/offset exact at column 1. See
`../POSITION_FINDINGS.md`.

## consumer usage (sparkdown as a unified parser)

`sparkdown.mjs` wraps the wasm as a unified parser plugin (drop-in for
`remark-parse`). `usage_demo.mjs` shows the real consumer flow:

```bash
node harness/usage_demo.mjs
```

```js
unified().use(sparkdown).use(remarkToc).use(remarkRehype).use(rehypeStringify).process(md)
```

Result: identical HTML to the `remark-parse` pipeline in **650/652 (99.7%)** of the
corpus. See `../INTEGRATION_FINDINGS.md`.
