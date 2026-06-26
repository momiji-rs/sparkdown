# SPIKE: mdast ecosystem-compatibility harness

**Question:** can sparkdown emit a tree that actually plugs into unified/remark
(MDAST → HAST → HTML, remark plugins)? How do we *verify* it, not just claim it?

**Method:** added an opt-in path that emits real nested **mdast** (blocks + inline
nodes, with link urls / code values captured at parse time — not scraped back out
of lossy HTML), serialized it for all 652 CommonMark examples, and ran it through
the **actual ecosystem packages** (`mdast-util-assert`,
`mdast-util-from-markdown`, `mdast-util-to-hast`, `hast-util-to-html`,
`mdast-util-to-string`, `unist-util-visit`). See `harness/`.

## Result (652 CommonMark examples)

| check    | pass | % | proves |
| -------- | ---- | --- | ------ |
| valid    | 652/652 | 100.0% | legal mdast (`mdast-util-assert` accepts every tree) |
| shape    | 537/652 | 82.4% | byte-identical tree to remark's own parser (ignoring `position`) |
| **rt-ref** | **651/652** | **99.8%** | **renders identically to the reference tree via the same to-hast/to-html** |
| rt-cmark | 541/652 | 83.0% | renders to cmark's exact expected HTML |
| plugin   | 652/652 | 100.0% | traversable + consumable by remark-style utils |

## Reading

- **rt-ref 99.8% is the headline.** Feed our tree and remark's own tree through
  the *same* renderer (`mdast-util-to-hast` → `hast-util-to-html`) and the HTML is
  identical for 651/652 examples. Semantically, **our mdast is interchangeable
  with remark's** — which is exactly what "plugs into the ecosystem" means.
- **valid + plugin 100%.** Every tree is legal mdast and is traversed/serialized
  by real unified utilities without error.
- **The 115 shape diffs are mostly explainable, not bugs:**
  - **77** are **link/image reference definitions.** remark keeps a `definition`
    node + `linkReference`/`imageReference`; sparkdown resolves references to
    `link`/`image` inline and drops the definition. A deliberate difference — and
    it still renders identically (counts in rt-ref's 99.8%). Matching remark here
    is a choice (emit `definition` + `*Reference` nodes), not a fix.
  - **~29** are list/listItem **`spread`** granularity. mdast separates
    `list.spread` (blank lines *between items*) from `listItem.spread` (blank
    lines *within an item*); sparkdown's block parser only exposes CommonMark's
    single combined `tight` bit. Exact parity needs finer blank-line info from the
    block layer.
  - a handful of edge cases (e.g. the one rt-ref miss: nested-image alt
    flattening `![a ![b]()]()` → alt loses the inner image's text).

## What this establishes

1. **The interchange works today.** With ~no semantic loss, sparkdown can act as
   a drop-in mdast source for the remark/rehype rendering pipeline.
2. **The verification method is the deliverable.** This harness is a repeatable,
   spec-corpus-wide gate: any change to the AST emitter is scored against the real
   ecosystem. It pinpoints the exact remaining gaps.
3. **The remaining gaps are known and bounded:** (a) reference-definition nodes
   (a modelling choice), (b) `spread` granularity (needs block-parser support),
   (c) `position` info (omitted in the spike; some plugins want it — next step).

## Caveats / scope

- Pure CommonMark (default build); GFM (`delete`/tables/footnotes) not exercised.
- `position` (line/column/offset) is not emitted yet. Many remark plugins read it
  (`remark-lint`, source-mapping). Our zero-copy spans map naturally to offsets,
  so this is wiring, not redesign — but it is unverified here.
- The mdast emitter is opt-in (`--features ast`); the default build stays
  byte-identical and zero-dependency (conformance still 652/652).

## Verdict

"Can we plug into the MDAST/HAST + JS plugin ecosystem?" — **yes, demonstrably:**
99.8% render-identical to remark's own tree, 100% valid and plugin-traversable.
The path to full shape-parity is a short, known list. The real remaining unknown
for a *shipping* product is still the **JS/wasm boundary** (serializing this tree
across to JS efficiently) — the next spike.
