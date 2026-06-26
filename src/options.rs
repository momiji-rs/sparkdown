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
    /// Emoji shortcodes: inline `:smile:` → 😄 (the remark-gemoji dataset).
    /// Effective only with the `emoji` Cargo feature (it carries the data table).
    pub emoji: bool,
    /// External-link transform (the rehype-external-links default): add
    /// `rel="nofollow"` to every link whose href begins with `http://`/`https://`
    /// (inline links and autolinks alike; `mailto:`/relative/fragment untouched).
    /// A post-render pass — `<a href="` only ever appears in real link tags.
    pub external_links: bool,
    /// GFM footnotes: inline `[^label]` references and `[^label]: …` block
    /// definitions (the remark-gfm grammar). References resolve only when a
    /// matching definition exists. mdast: `footnoteReference`/`footnoteDefinition`;
    /// HTML: the remark-rehype footnotes `<section>` with numbered backrefs. The
    /// label set is collected in the block pass, so forward references work.
    pub footnotes: bool,
    /// Definition lists (the pandoc / remark-definition-list grammar): a term
    /// paragraph followed by one or more `: definition` lines becomes
    /// `<dl><dt>…</dt><dd>…</dd></dl>`. A blank line before a `:` marker makes
    /// that definition loose (its body is wrapped in `<p>`). Not GFM-gated.
    pub deflist: bool,
    /// Directives (the remark-directive grammar): inline `:name[label]{attrs}`
    /// (text), `::name[label]{attrs}` (leaf), and `:::name[label]{attrs}` … `:::`
    /// (container). mdast emits `textDirective`/`leafDirective`/`containerDirective`
    /// with a `name` and an `attributes` object (the canonical, gate-able output);
    /// HTML follows the common convention (the name becomes the element, with
    /// `#id`/`.class`/`key=val` as its attributes). Not GFM-gated.
    pub directives: bool,
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

    /// `true` iff the `emoji` Cargo feature is compiled in. Used as
    /// `Options::EMOJI && opts.emoji` so the lookup and its data table fold away
    /// entirely without the feature.
    #[cfg(feature = "emoji")]
    pub(crate) const EMOJI: bool = true;
    #[cfg(not(feature = "emoji"))]
    pub(crate) const EMOJI: bool = false;

    /// `true` iff the `footnotes` Cargo feature is compiled in. Used as
    /// `Options::FOOTNOTES && opts.footnotes` so that, without the feature, every
    /// footnote check folds to `false` and its code is eliminated — the default
    /// build is byte-for-byte the pure-CommonMark fast path.
    #[cfg(feature = "footnotes")]
    pub(crate) const FOOTNOTES: bool = true;
    #[cfg(not(feature = "footnotes"))]
    pub(crate) const FOOTNOTES: bool = false;

    /// `true` iff the `deflist` Cargo feature is compiled in. Used as
    /// `Options::DEFLIST && opts.deflist` so that, without the feature, every
    /// definition-list check folds to `false` and its code is eliminated — the
    /// default build is byte-for-byte the pure-CommonMark fast path.
    #[cfg(feature = "deflist")]
    pub(crate) const DEFLIST: bool = true;
    #[cfg(not(feature = "deflist"))]
    pub(crate) const DEFLIST: bool = false;

    /// `true` iff the `directives` Cargo feature is compiled in. Used as
    /// `Options::DIRECTIVES && opts.directives` so that, without the feature, every
    /// directive check folds to `false` and its code is eliminated — the default
    /// build is byte-for-byte the pure-CommonMark fast path.
    #[cfg(feature = "directives")]
    pub(crate) const DIRECTIVES: bool = true;
    #[cfg(not(feature = "directives"))]
    pub(crate) const DIRECTIVES: bool = false;

    /// `true` iff the `frontmatter` Cargo feature is compiled in. Used as
    /// `Options::FRONTMATTER && opts.frontmatter` so that, without the feature, the
    /// frontmatter check folds to `false` and its code is eliminated — the default
    /// build is byte-for-byte the pure-CommonMark fast path.
    #[cfg(feature = "frontmatter")]
    pub(crate) const FRONTMATTER: bool = true;
    #[cfg(not(feature = "frontmatter"))]
    pub(crate) const FRONTMATTER: bool = false;

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
            emoji: false,
            external_links: false,
            footnotes: false,
            deflist: false,
            directives: false,
        }
    }
}
