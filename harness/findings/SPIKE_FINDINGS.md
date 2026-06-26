# SPIKE: AST-path tax

**Question:** if we add a programmable MDAST (so we can offer a plugin system
like Sätteri / remark-rehype), how much speed do we lose vs the direct
source→HTML fast path?

**Method:** opt-in `ast` feature. `to_ast()` materializes a full owned tree —
blocks **and** real inline nodes (text/emphasis/link tokens, tapped from the
resolved inline slot list, not a cheap approximation). `render_ast()` renders it
back. The bench asserts `render_ast(to_ast(src)) == to_html(src)` **byte-for-byte**,
so the tree provably holds the real workload. Manual timing, CommonMark spec
fixture (~197 KB), Apple Mac Studio, `--features ast --bench ast_tax`.

## Result

| path                       | µs/op | vs fast |
| -------------------------- | ----: | ------: |
| fast (`to_html`)           | ~580  |  1.00×  |
| **ast total (build+render)** | **~800** | **~1.39×** |
| · ast build (`to_ast`)     | ~590  |  ~1.01× |
| · ast render (`render_ast`)| ~158  |  ~0.27× |

Materialized nodes: **4515** for 197 KB.

## Reading

- **A full owned-AST round-trip costs ~1.4× the fast path.** Cheaper than feared:
  the expensive part (SIMD text scan + inline parse + HTML escaping) is *shared*,
  so the tax is only node allocation + owned-string copies + a second traversal.
- **The tax is in materialization, not traversal.** Building the tree ≈ 1.0× a
  whole `to_html`; rendering *from* it is only 0.27×. So a visitor/plugin model is
  cheap at the margin: build once (~1.0×), then each pure walk pass is ≤ ~0.27×,
  then render (~0.27×). N plugins ≈ `1.0× + N·(≤0.27×) + 0.27×`.
- **The core-parse speed edge is preserved.** Sätteri also materializes an arena
  MDAST/HAST; the parse underneath it is pulldown-cmark. Our parse is faster and
  is shared by both our paths, so an AST layer doesn't surrender the advantage.

## Caveats (true tax is somewhat higher than 1.4×)

1. Inline nodes here are a **flat token stream** (`Text`/`Tag`), not a **nested**
   semantic tree (`Emph{children}`, `Link{dest,children}`, parent pointers). A
   real MDAST allocates more per-node `Vec`s → more build cost. 1.4× is a
   realistic **lower bound**.
2. This is a **Rust-native** AST. The actual competitive feature — *JS* plugins —
   adds the wasm/napi boundary (serialize tree out, mutate in JS, read back).
   That boundary, not the Rust materialization, is the real cost center and is
   **not** measured here. It is the next spike if we pursue the ecosystem play.
3. Default build is untouched: all `ast` code is `#[cfg(feature = "ast")]`;
   conformance stays 652/652 and `to_html` keeps the byte-identical fast path.

## Verdict

Materializing a programmable AST in Rust is affordable (~1.4×, opt-in). The open
risk for the "JS plugin ecosystem" ambition is the **language boundary**, not the
tree. Recommend the next spike measure an event-stream/batch-serialized AST across
the wasm boundary before committing to the design.
