//! Opt-in parser/renderer options. The default (`Options::default()`, all off)
//! is the tuned pure-CommonMark fast path; every flag is checked at a block- or
//! inline-*type* boundary (never per byte), so disabled extensions cost nothing.

/// Feature flags layered on top of CommonMark 0.31.2. All default to off.
///
/// ```
/// use sparkdown::{Options, to_html_with};
/// let opts = Options { hard_wraps: true, ..Options::default() };
/// assert_eq!(to_html_with("a\nb", &opts), "<p>a<br />\nb</p>\n");
/// ```
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Options {
    /// GFM `~~strikethrough~~` → `<del>`.
    pub strikethrough: bool,
    /// GFM task list items: `- [ ]` / `- [x]`.
    pub tasklist: bool,
    /// GFM extended autolinks (bare `www.`/`http(s)://`/email).
    pub autolink: bool,
    /// GFM tag filter — neutralize a few raw-HTML tags (`<script>`, …).
    pub tagfilter: bool,
    /// GFM pipe tables.
    pub tables: bool,
    /// Render every soft line break as `<br />` (goldmark's `WithHardWraps`).
    pub hard_wraps: bool,
    /// Diagram extension: render `mermaid` fenced code blocks as a client-side
    /// `<pre class="mermaid">` wrapper. Effective only with the `diagram` feature.
    pub diagram: bool,
    /// Built-in transform (PROTOTYPE): emit a github-slugger-style `id` on every
    /// heading during the render walk (the Rust equivalent of rehype-slug), so the
    /// common "headings get anchors" task stays on the all-wasm fast path instead
    /// of crossing into a JS plugin. Applied in `render.rs`; not GFM-gated.
    pub heading_ids: bool,
    /// Frontmatter: a YAML (`---`) or TOML (`+++`) fenced block at the very start
    /// of the document (the remark-frontmatter grammar). Renders to nothing; with
    /// the `ast` feature it becomes a `yaml`/`toml` mdast node. Not GFM-gated.
    pub frontmatter: bool,
    /// GFM footnotes: inline `[^label]` references and `[^label]: …` block
    /// definitions (the remark-gfm grammar). References resolve only when a
    /// matching definition exists. mdast: `footnoteReference`/`footnoteDefinition`;
    /// HTML: the remark-rehype footnotes `<section>` with numbered backrefs. The
    /// label set is collected in the block pass, so forward references work.
    pub footnotes: bool,
}

impl Options {
    /// `true` iff the `gfm` Cargo feature is enabled (GFM code is compiled in).
    /// Used as `Options::GFM && opts.flag` so that, without the feature, every
    /// GFM check folds to `false` and its code is eliminated — the default build
    /// is byte-for-byte the pure-CommonMark fast path.
    #[cfg(feature = "gfm")]
    pub(crate) const GFM: bool = true;
    #[cfg(not(feature = "gfm"))]
    pub(crate) const GFM: bool = false;

    /// `true` iff the `diagram` Cargo feature is compiled in. Used as
    /// `Options::DIAGRAM && opts.diagram` so that, without the feature, the check
    /// folds to `false` and the diagram code is eliminated — the default build is
    /// byte-for-byte the pure-CommonMark fast path. (The per-extension template.)
    #[cfg(feature = "diagram")]
    pub(crate) const DIAGRAM: bool = true;
    #[cfg(not(feature = "diagram"))]
    pub(crate) const DIAGRAM: bool = false;

    /// GitHub Flavored Markdown: every GFM extension enabled. (Effective only
    /// when the crate is built with the `gfm` feature.)
    pub const fn gfm() -> Self {
        Options {
            strikethrough: true,
            tasklist: true,
            autolink: true,
            tagfilter: true,
            tables: true,
            hard_wraps: false,
            diagram: false,
            heading_ids: false,
            frontmatter: false,
            footnotes: false,
        }
    }
}
