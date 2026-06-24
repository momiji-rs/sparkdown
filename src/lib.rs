//! sparkdown — a fast, **standards-first** CommonMark parser in Rust.
//!
//! *markdown, with a spark.* Where [rostdown](https://github.com/momiji-rs/rostdown)
//! renders a kramdown **subset** and cleanly declines everything else,
//! sparkdown's contract is the inverse:
//! handle the **whole** [CommonMark 0.31.2](https://spec.commonmark.org/0.31.2/)
//! grammar and aim for byte-identical output with the reference `cmark`.
//! The bar to beat on speed is `pulldown-cmark`.
//!
//! ## Provenance
//!
//! The performance substrate is lifted verbatim from rostdown — it is
//! grammar-agnostic and already battle-tested:
//!
//! - [`scan`] — SWAR byte search (zero-dep, no `unsafe`).
//! - [`bump`] — the always-on local bump arena.
//! - [`arena`] — the opt-in `ScopedAlloc` global allocator (`arena` feature).
//! - [`entities`] — HTML entity tables.
//!
//! The grammar layer ([`parse`], [`render`]) is **new**: rostdown's parser
//! is built around "decline if outside my subset", the exact opposite of
//! what a full CommonMark parser needs.
//!
//! ## Status — SCAFFOLD
//!
//! Stage 1: only blank-line-separated paragraphs with HTML-escaped text are
//! implemented. The live conformance number against the official 652-example
//! CommonMark suite is the test `commonmark_conformance` (`cargo test --
//! --nocapture`). Everything below is the build-out surface.
#![allow(dead_code)] // SCAFFOLD: the vendored perf primitives (bump, arena,
// entities, and the wider scan API) are present but not all wired yet.
// Remove this once the block + inline parser lands.

#[cfg(feature = "arena")]
mod arena;
mod block;
mod bump;
mod entities;
mod inline;
mod render;
mod scan;

#[cfg(feature = "arena")]
pub use arena::ScopedAlloc;

/// Render CommonMark `src` to HTML.
///
/// **Scaffold status:** parses blank-line-separated paragraphs only and
/// HTML-escapes their text. No inline parsing, no other block types yet.
pub fn to_html(src: &str) -> String {
    let tree = block::parse(src);
    render::render(&tree)
}

/// Diagnostic re-exports so a profiler can wrap each phase. Not public API.
#[cfg(feature = "profiling")]
pub mod prof {
    pub use crate::block::{parse, Tree};
    pub use crate::render::render;
}

/// Diagnostic: time the parse phase and the render phase separately, returning
/// `(parse_ns_per_op, render_ns_per_op)`. Not part of the public API.
#[cfg(feature = "profiling")]
pub fn profile_phases(src: &str, iters: u32) -> (f64, f64) {
    use std::time::Instant;
    let warm = (iters / 5).max(3);

    for _ in 0..warm {
        std::hint::black_box(block::parse(src));
    }
    let t = Instant::now();
    for _ in 0..iters {
        std::hint::black_box(block::parse(src));
    }
    let parse_ns = t.elapsed().as_nanos() as f64 / iters as f64;

    let tree = block::parse(src);
    for _ in 0..warm {
        std::hint::black_box(render::render(&tree));
    }
    let t = Instant::now();
    for _ in 0..iters {
        std::hint::black_box(render::render(&tree));
    }
    let render_ns = t.elapsed().as_nanos() as f64 / iters as f64;
    (parse_ns, render_ns)
}

#[cfg(test)]
mod smoke {
    use super::*;

    #[test]
    fn paragraphs_and_escaping() {
        assert_eq!(to_html("aaa\n\nbbb\n"), "<p>aaa</p>\n<p>bbb</p>\n");
        assert_eq!(to_html("a < b & c\n"), "<p>a &lt; b &amp; c</p>\n");
        assert_eq!(to_html(""), "");
    }
}
