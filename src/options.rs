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
}

impl Options {
    /// GitHub Flavored Markdown: every GFM extension enabled.
    pub const fn gfm() -> Self {
        Options {
            strikethrough: true,
            tasklist: true,
            autolink: true,
            tagfilter: true,
            tables: true,
            hard_wraps: false,
        }
    }
}
