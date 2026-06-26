# SPIKE: the wasm→JS boundary

**Question:** the premise of a "Rust core + JS plugins" product (Sätteri's pitch)
is that parsing in Rust/wasm and shipping the tree to JS beats parsing in JS. Is
that true — or does serializing the AST across the boundary eat the win?

**Method:** added a `sparkdown_to_mdast_json` wasm export (parse + serialize mdast
to JSON, zero-dep). The JS host writes source into linear memory, calls it, reads
the bytes back, `TextDecoder` + `JSON.parse` → a remark-shaped mdast tree. Compared
end-to-end against `mdast-util-from-markdown` (what remark/unified actually uses).
Optimized wasm (`+simd128 +bulk-memory`, `wasm-opt -O4`), Node 22 / V8, warm
instance. See `harness/wasm_boundary.mjs` and `harness/wasm_sweep.mjs`.

## Result — CommonMark spec doc (198 KB)

Both paths end with an equivalent JS mdast tree (1416 nodes either way).

| path                                       | µs/op | vs JS |
| ------------------------------------------ | ----: | ----: |
| JS: `mdast-util-from-markdown` → tree      | ~73000 | 1.00× |
| **wasm: → JS mdast tree (TOTAL)**          | **~3100** | **0.04× (≈23× faster)** |
| · wasm call (parse+serialize+boundary)     | ~1820 | |
| · + TextDecoder (bytes→JS string)          | ~2080 | |
| · JSON.parse (string→JS objects)           | ~910  | |
| · [ref] wasm `to_html` (parse only)        | ~920  | |

mdast JSON size: 296 KB.

## Holds across doc sizes

| doc | JS µs | wasm µs | speedup |
| --- | ----: | ------: | ------: |
| 1 KB | 166 | 13 | 13× |
| 5 KB | 1090 | 59 | 19× |
| 20 KB | 5710 | 306 | 19× |
| 100 KB | 36047 | 1600 | 23× |
| 198 KB | 71604 | 3111 | 23× |

## Reading

- **The premise holds, decisively.** Even with the "naive" JSON boundary —
  serialize to JSON in Rust, copy out, `TextDecoder`, `JSON.parse` — the wasm path
  produces the JS mdast tree **13–23× faster** than parsing in JS. The boundary is
  not the bottleneck; **JS parsing is.**
- **Where the time goes.** Pure wasm parse is ~0.9 ms; mdast JSON serialize adds
  ~0.9 ms; decode ~0.3 ms; `JSON.parse` ~0.9 ms. So the boundary roughly triples
  parse cost — and still beats JS parsing by an order of magnitude. (For context,
  `mdast-util-from-markdown` on this doc is ~73 ms; micromark is correctness-first,
  not speed-first.)
- **The fancy boundary is unnecessary.** Sätteri uses an arena binary AST to avoid
  serialization. A flat-binary / lazy-materialization boundary *would* shave the
  ~0.9 ms `JSON.parse` + ~0.9 ms serialize — but JSON already wins 23×, so this is
  an optimization, not a requirement. Ship JSON first.
- **Plugins are free of this.** Both paths hand you a normal JS mdast tree; plugin
  traversal cost is identical afterward. The win is entirely in *getting* the tree.

## Caveats

- Measured on Node 22 / V8 and one corpus (the CommonMark spec doc + prefixes).
  Browser engines are the same ballpark but unverified here.
- Warm wasm instance; module instantiation (~ms) is a one-time cost, amortized by
  any server / long-lived page.
- `JSON.parse` cost scales with tree size; very large docs shift more weight onto
  it (still dominated by the JS-parse gap). Flat-binary is the lever if ever needed.
- mdast shape parity is 82% / render-parity 99.8% (see HARNESS_FINDINGS.md); the
  boundary spike reuses that same emitter, so it carries the same known gaps
  (reference definitions, list `spread`, no `position`).

## Verdict

The last open risk for a "sparkdown core + JS plugin ecosystem" play is cleared:
**the language boundary is cheap.** Parsing in wasm and shipping JSON to JS is
~13–23× faster than the incumbent JS parser, across sizes, with a trivial ABI and
zero extra dependencies. The path to a product is now an engineering/ergonomics
question (plugin API, `position`, reference nodes), not a feasibility one.
