//! Diagram extension (render-only, zero-dependency).
//!
//! A fenced code block whose info string names a client-side diagram language —
//! currently `mermaid` — is emitted as a bare `<pre class="mermaid">…</pre>`
//! wrapper instead of `<pre><code>`, so a browser library (mermaid.js) renders
//! it. sparkdown does not draw the diagram; it only shapes the output element.
//! The diagram source is HTML-escaped (the browser un-escapes `textContent`, so
//! `-->` round-trips correctly).

use crate::block::Tree;
use crate::render::escape_html;

/// Info strings (first word) that render as a client-side diagram wrapper.
const DIAGRAM_LANGS: &[&str] = &["mermaid"];

/// If the fenced-code block `idx` is tagged with a diagram language, emit its
/// client-side wrapper into `out` and return `true`. Otherwise emit nothing and
/// return `false` — the caller then renders the standard `<pre><code>`.
pub(crate) fn try_render(tree: &Tree, idx: usize, out: &mut String) -> bool {
    let Some(lang) = tree.info(idx).split_whitespace().next() else {
        return false;
    };
    if !DIAGRAM_LANGS.contains(&lang) {
        return false;
    }
    out.push_str("<pre class=\"");
    out.push_str(lang);
    out.push_str("\">");
    escape_html(tree.content(idx), out);
    out.push_str("</pre>");
    true
}
