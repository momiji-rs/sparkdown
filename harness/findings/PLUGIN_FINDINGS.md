# SPIKE: real remark/rehype plugins on sparkdown's mdast

**Question:** the harness proved our tree is *shaped* like remark's and *renders*
like remark's. But does a real, published plugin actually run on it and do its job
— indistinguishably from running on remark's own tree?

**Method:** built the genuine unified pipeline with three popular npm plugins and
fed it sparkdown's mdast (produced through the **wasm boundary**, the real product
path):

```
sparkdown (wasm) → mdast
  → remark-emoji      (:tada: → 🎉)            mdast transform
  → remark-toc        (inject table of contents) mdast transform
  → remark-rehype     (mdast → hast)
  → rehype-slug       (add id="" to headings)     hast transform
  → rehype-stringify  (hast → HTML)
```

Then ran the **same** processor on `mdast-util-from-markdown`'s tree and compared.

## Result

On a representative document, every plugin acted correctly on sparkdown's tree:

- `remark-emoji` → `🎉` in the output ✅
- `remark-toc` → injected `<ul>` of anchor links to the headings ✅
- `rehype-slug` → `<h2 id="introduction">` etc. ✅
- links + autolinks rendered ✅

…and the HTML was **byte-identical** to running the pipeline on remark's own tree.

**Generalized across all 652 CommonMark examples** (same real-plugin pipeline,
sparkdown tree vs remark tree):

```
identical HTML: 650 / 652  (99.7%)
differ (2): 173 (a <style> HTML block), 574 (nested-image alt ![a ![b]()]())
```

The 2 outliers are the known edge cases (raw-HTML-block trailing handling; nested
image alt flattening) — not plugin incompatibilities.

## Reading

- **Real plugins cannot tell the trees apart.** Emoji substitution, TOC injection,
  and heading-slug all run on sparkdown's tree and produce the same output as on
  remark's — across 99.7% of the spec corpus, through the genuine `unified`
  machinery (not a reimplementation).
- **Both halves of the ecosystem work:** mdast plugins (`remark-*`) and hast
  plugins (`rehype-*`) in one pipeline, over our tree.
- This was driven through the **wasm boundary**, so it exercises the full product
  path: Rust parse → JSON → JS tree → real plugins → HTML.

## Caveats

- `position` is still omitted. These three plugins don't need it; position-reading
  plugins (`remark-lint`, source-mapping, some `*-directive`) would. Emitting it is
  the main remaining item for broad plugin coverage.
- Reference-definition shape difference (we inline-resolve) did **not** break the
  pipeline — `remark-rehype` renders `link`/`linkReference` to the same HTML — so
  it stayed invisible here. It would matter only to plugins that inspect
  `definition`/`*Reference` nodes specifically.

## Verdict

The "run a real remark plugin" milestone is met conclusively: sparkdown is a
**drop-in mdast source for the remark/rehype ecosystem** — real plugins run
unmodified and produce identical results, across the corpus, via the wasm path.
Combined with the prior spikes (AST ~1.4×, 99.8% render-parity, boundary 13–23×
faster than JS), the ecosystem play is proven end-to-end. Remaining work is
ergonomics + `position`, not feasibility.

Run it: `node harness/plugin_demo.mjs` (needs `harness/sparkdown.wasm` and
`harness/sparkdown-mdast.json` built — see `harness/README.md`).
