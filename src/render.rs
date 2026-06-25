//! HTML renderer — walks the block [`Tree`] emitting CommonMark HTML.
//!
//! Newline placement follows the reference renderer's `cr()` convention (emit
//! a newline only when not already at line start), which reproduces cmark's
//! exact whitespace, including tight-vs-loose list items.

use crate::block::{Kind, Tree};
use crate::inline::{Scratch, render_inline};
use crate::scan::escape_block_mask;

/// Render a parsed [`Tree`] to an HTML string.
pub fn render(tree: &Tree) -> String {
    // Size the buffer from the input. A tighter estimate beats a generous one:
    // over-reserving just page-faults more freshly-allocated memory on first
    // write (measured slower) than absorbing the occasional grow.
    let mut out = String::with_capacity(tree.source_len + tree.source_len / 8 + 64);
    let mut scratch = Scratch::new();
    children(tree, tree.root, &mut out, &mut scratch);
    out
}

/// Emit a newline unless `out` is empty or already ends with one. One byte
/// check (the last byte) instead of `is_empty()` + the `ends_with` machinery —
/// `cr` runs several times per node.
fn cr(out: &mut String) {
    match out.as_bytes().last() {
        None | Some(b'\n') => {}
        _ => out.push('\n'),
    }
}

fn children(tree: &Tree, idx: usize, out: &mut String, scratch: &mut Scratch) {
    // Walk the intrusive first-child/next-sibling list.
    let mut c = tree.first_child(idx);
    while let Some(ci) = c {
        render_node(tree, ci, out, scratch);
        c = tree.next_sibling(ci);
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
fn escape_entity(b: u8) -> &'static str {
    match b {
        b'&' => "&amp;",
        b'<' => "&lt;",
        b'>' => "&gt;",
        b'"' => "&quot;",
        _ => unreachable!("only &, <, >, \" are escaped"),
    }
}

pub fn escape_html(s: &str, out: &mut String) {
    let bytes = s.as_bytes();
    // Pending clean run [clean, i): emitted lazily (one push_str) when a special
    // is hit or at the end, so consecutive clean blocks coalesce into one copy.
    let mut clean = 0;
    let mut i = 0;
    // Fused SIMD: one compare per 16 bytes yields every special in the block.
    while i + 16 <= bytes.len() {
        let mut mask = escape_block_mask(bytes, i);
        while mask != 0 {
            let hit = i + mask.trailing_zeros() as usize;
            out.push_str(&s[clean..hit]);
            out.push_str(escape_entity(bytes[hit]));
            clean = hit + 1;
            mask &= mask - 1; // clear the lowest set bit
        }
        i += 16;
    }
    while i < bytes.len() {
        if matches!(bytes[i], b'&' | b'<' | b'>' | b'"') {
            out.push_str(&s[clean..i]);
            out.push_str(escape_entity(bytes[i]));
            clean = i + 1;
        }
        i += 1;
    }
    out.push_str(&s[clean..]);
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
