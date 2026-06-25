//! sparkdown — a fast, **standards-first** CommonMark parser in Rust.
//!
//! *markdown, with a spark.* Where [rostdown](https://github.com/momiji-rs/rostdown)
//! renders a kramdown **subset** and cleanly declines everything else,
//! sparkdown's contract is the inverse:
//! handle the **whole** [CommonMark 0.31.2](https://spec.commonmark.org/0.31.2/)
//! grammar and aim for byte-identical output with the reference `cmark`.
//! It's built first for ourselves: a fast, dependency-free engine that's
//! pleasant to embed.
//!
//! ## Provenance
//!
//! The performance substrate is lifted verbatim from rostdown — it is
//! grammar-agnostic and already battle-tested:
//!
//! - `scan` — SWAR byte search (zero-dep, no `unsafe`).
//! - `bump` — the always-on local bump arena.
//! - `arena` — the opt-in `ScopedAlloc` global allocator (`arena` feature).
//! - `entities` — HTML entity tables.
//!
//! The grammar layer (`block`, `render`) is **new**: rostdown's parser
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
mod options;
mod render;
mod scan;
#[cfg(feature = "wasm")]
mod wasm;

pub use options::Options;

#[cfg(feature = "arena")]
pub use arena::ScopedAlloc;

/// Render CommonMark `src` to HTML (full CommonMark 0.31.2).
///
/// For opt-in GFM/extensions use [`to_html_with`]; for repeated rendering use a
/// reusable [`Renderer`].
pub fn to_html(src: &str) -> String {
    render_html(src, Options::default())
}

/// Render `src` to HTML with opt-in [`Options`] (GFM, hard wraps, …). With
/// `Options::default()` this is exactly [`to_html`] — disabled features are
/// resolved once per render, never in the byte loop.
pub fn to_html_with(src: &str, opts: &Options) -> String {
    render_html(src, *opts)
}

#[cfg(not(feature = "arena"))]
fn render_html(src: &str, opts: Options) -> String {
    render::render(&block::parse_with_opts(src, opts))
}

/// With the `arena` feature (and [`ScopedAlloc`] installed as the
/// `#[global_allocator]`), the whole parse+render runs inside a bump scope:
/// every intermediate allocation is a pointer bump and is freed wholesale. The
/// result `String` is copied to the system allocator before the reset.
#[cfg(feature = "arena")]
fn render_html(src: &str, opts: Options) -> String {
    let guard = arena::Scope::enter();
    let html = render::render(&block::parse_with_opts(src, opts));
    let outermost = arena::leave_no_reset();
    core::mem::forget(guard); // we left manually
    if outermost {
        let owned = String::from(html.as_str()); // depth 0 → System
        drop(html); // arena dealloc is a no-op
        arena::reset();
        owned
    } else {
        html // nested in an outer scope that owns the arena lifetime
    }
}

/// A reusable rendering context that keeps the working buffers (node arena,
/// text buffer, reference map, inline scratch, and output) warm across renders.
///
/// Use it instead of [`to_html`] for repeated rendering — a server, or a
/// long-lived wasm instance handling many documents — to avoid re-allocating
/// those buffers on every call.
///
/// ```
/// let mut r = sparkdown::Renderer::new();
/// assert_eq!(r.render("# a"), "<h1>a</h1>\n");
/// assert_eq!(r.render("*b*"), "<p><em>b</em></p>\n"); // reuses the buffers
/// ```
pub struct Renderer {
    scratch: inline::Scratch,
    out: String,
    nodes: Vec<block::Node>,
    buf: String,
    refmap: inline::RefMap,
    opts: Options,
}

impl Renderer {
    /// Create an empty context (CommonMark, no options); its buffers grow to
    /// fit the first render and are reused thereafter.
    pub fn new() -> Self {
        Self::with_options(Options::default())
    }

    /// Create a context with opt-in [`Options`] applied to every render.
    pub fn with_options(opts: Options) -> Self {
        Renderer {
            scratch: inline::Scratch::new(),
            out: String::new(),
            nodes: Vec::new(),
            buf: String::new(),
            refmap: inline::RefMap::new(),
            opts,
        }
    }

    /// Render CommonMark `src` to HTML, reusing the held buffers. The returned
    /// string borrows the context until the next call.
    pub fn render(&mut self, src: &str) -> &str {
        self.out.clear();
        let nodes = core::mem::take(&mut self.nodes);
        let buf = core::mem::take(&mut self.buf);
        let refmap = core::mem::take(&mut self.refmap);
        let tree = block::parse_with(src, self.opts, nodes, buf, refmap);
        render::render_with(&tree, &mut self.out, &mut self.scratch);
        (self.nodes, self.buf, self.refmap) = tree.recycle();
        &self.out
    }
}

impl Default for Renderer {
    fn default() -> Self {
        Self::new()
    }
}

/// Diagnostic re-exports so a profiler can wrap each phase. Not public API.
#[cfg(feature = "profiling")]
pub mod prof {
    pub use crate::block::{Tree, parse};
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
