//! HTML renderer — walks the block [`Tree`] emitting CommonMark HTML.
//!
//! Newline placement follows the reference renderer's `cr()` convention (emit
//! a newline only when not already at line start), which reproduces cmark's
//! exact whitespace, including tight-vs-loose list items.

use crate::block::{Kind, Tree};
use crate::inline::{Scratch, render_inline};
use crate::scan::find_escape;

/// Render a parsed [`Tree`] to an HTML string.
pub fn render(tree: &Tree) -> String {
    // Size the buffer from the input so the common case grows rarely.
    let mut out = String::with_capacity(tree.source_len + tree.source_len / 8 + 64);
    let mut scratch = Scratch::new();
    for &c in &tree.nodes[tree.root].children {
        render_node(tree, c, &mut out, &mut scratch);
    }
    out
}

/// Emit a newline unless `out` already ends with one.
fn cr(out: &mut String) {
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
}

fn children(tree: &Tree, idx: usize, out: &mut String, scratch: &mut Scratch) {
    // `tree` is a shared borrow; iterating its children while recursing needs
    // no clone (both are shared borrows).
    for &c in &tree.nodes[idx].children {
        render_node(tree, c, out, scratch);
    }
}

fn render_node(tree: &Tree, idx: usize, out: &mut String, scratch: &mut Scratch) {
    let node = &tree.nodes[idx];
    match node.kind {
        Kind::Document => children(tree, idx, out, scratch),
        Kind::Paragraph => {
            let tight = in_tight_list(tree, idx);
            if !tight {
                cr(out);
                out.push_str("<p>");
            }
            render_inline(tree.content(idx), out, &tree.refmap, scratch);
            if !tight {
                out.push_str("</p>");
                cr(out);
            }
        }
        Kind::Heading => {
            let level = node.level;
            cr(out);
            out.push_str("<h");
            out.push((b'0' + level) as char);
            out.push('>');
            render_inline(tree.content(idx), out, &tree.refmap, scratch);
            out.push_str("</h");
            out.push((b'0' + level) as char);
            out.push('>');
            cr(out);
        }
        Kind::ThematicBreak => {
            cr(out);
            out.push_str("<hr />");
            cr(out);
        }
        Kind::CodeBlock => {
            cr(out);
            out.push_str("<pre><code");
            if let Some(word) = tree.info(idx).split_whitespace().next() {
                out.push_str(" class=\"language-");
                escape_html(crate::inline::unescape_string(word).as_ref(), out);
                out.push('"');
            }
            out.push('>');
            escape_html(tree.content(idx), out);
            out.push_str("</code></pre>");
            cr(out);
        }
        Kind::HtmlBlock => {
            cr(out);
            out.push_str(tree.content(idx));
            cr(out);
        }
        Kind::BlockQuote => {
            cr(out);
            out.push_str("<blockquote>");
            cr(out);
            children(tree, idx, out, scratch);
            cr(out);
            out.push_str("</blockquote>");
            cr(out);
        }
        Kind::List => {
            let ld = node.list.as_ref().unwrap();
            cr(out);
            if ld.ordered {
                if ld.start == 1 {
                    out.push_str("<ol>");
                } else {
                    out.push_str("<ol start=\"");
                    out.push_str(&ld.start.to_string());
                    out.push_str("\">");
                }
            } else {
                out.push_str("<ul>");
            }
            children(tree, idx, out, scratch);
            cr(out);
            out.push_str(if ld.ordered { "</ol>" } else { "</ul>" });
            cr(out);
        }
        Kind::Item => {
            cr(out);
            out.push_str("<li>");
            children(tree, idx, out, scratch);
            out.push_str("</li>");
            cr(out);
        }
    }
}

/// A paragraph is rendered bare (no `<p>`) when it is a direct child of an item
/// in a tight list.
fn in_tight_list(tree: &Tree, para: usize) -> bool {
    let item = tree.nodes[para].parent;
    if tree.nodes[item].kind != Kind::Item {
        return false;
    }
    let list = tree.nodes[item].parent;
    tree.nodes[list].kind == Kind::List && tree.nodes[list].list.as_ref().is_some_and(|l| l.tight)
}

/// Append `s` to `out`, escaping the HTML-text specials `&`, `<`, `>`, `"`
/// (cmark escapes the double quote in text and code as well as attributes).
pub fn escape_html(s: &str, out: &mut String) {
    let bytes = s.as_bytes();
    let mut i = 0;
    while let Some(rel) = find_escape(&bytes[i..]) {
        let hit = i + rel;
        out.push_str(&s[i..hit]);
        out.push_str(match bytes[hit] {
            b'&' => "&amp;",
            b'<' => "&lt;",
            b'>' => "&gt;",
            b'"' => "&quot;",
            _ => unreachable!("find_escape only reports &, <, >, \""),
        });
        i = hit + 1;
    }
    out.push_str(&s[i..]);
}

#[cfg(test)]
mod tests {
    use super::escape_html;

    #[test]
    fn escapes_text_specials() {
        let mut out = String::new();
        escape_html("a < b & c > d", &mut out);
        assert_eq!(out, "a &lt; b &amp; c &gt; d");
    }
}
