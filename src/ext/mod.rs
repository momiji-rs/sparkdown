//! Opt-in extensions beyond CommonMark + GFM.
//!
//! Each extension lives in its own module, gated by **two** layers so it never
//! costs the default build a thing:
//!
//! 1. a **Cargo feature** (compile-time) — without it the module is not compiled
//!    and every call site folds away (`Options::FEATURE && …` → `false`), so the
//!    default build is byte-for-byte the pure-CommonMark fast path;
//! 2. an **`Options` flag** (run-time) — within a build that *did* compile the
//!    extension, the flag toggles it (a branch-predicted check at a block-/
//!    inline-*type* boundary, never per byte), so one binary serves many configs
//!    (e.g. the wasm flags bitmask).
//!
//! `diagram` is the worked template: a render-only hook (`render.rs`'s code-block
//! arm calls [`diagram::try_render`]). New extensions follow the same shape —
//! feature in `Cargo.toml`, field + `FEATURE` const in `options.rs`, a gated hook
//! in the relevant hot-path file, a module here, and a feature-gated test.

#[cfg(feature = "diagram")]
pub(crate) mod diagram;
