//! HTML renderer — walks the block [`Tree`] emitting CommonMark HTML.
//!
//! Newline placement follows the reference renderer's `cr()` convention (emit
//! a newline only when not already at line start), which reproduces cmark's
//! exact whitespace, including tight-vs-loose list items.

use crate::block::{Kind, Tree};
use crate::inline::{Scratch, encode_footnote_id, render_inline};
use crate::options::Options;
use crate::scan::{escape_block_mask, memchr1};
#[cfg(feature = "gfm")]
use std::borrow::Cow;

/// Render a parsed [`Tree`] to an HTML string.
pub fn render(tree: &Tree) -> String {
    // Size the buffer from the input. A tighter estimate beats a generous one:
    // over-reserving just page-faults more freshly-allocated memory on first
    // write (measured slower) than absorbing the occasional grow.
    let mut out = String::with_capacity(tree.source_len + tree.source_len / 8 + 64);
    let mut scratch = Scratch::new();
    render_with(tree, &mut out, &mut scratch);
    out
}

/// Render `tree` into a caller-owned buffer with a caller-owned scratch — both
/// reused across renders by [`crate::Renderer`]. The caller clears `out`.
pub(crate) fn render_with(tree: &Tree, out: &mut String, scratch: &mut Scratch) {
    // Per-document slug de-dup state for the built-in heading-id transform.
    if tree.opts.heading_ids {
        scratch.slugs.clear();
    }
    // Per-document footnote state: seed the reference set (so `[^x]` resolves) and
    // reset the numbering accumulated while rendering references.
    if tree.opts.footnotes {
        scratch.footnote_ids.clone_from(&tree.footnote_ids);
        scratch.footnote_order.clear();
        scratch.footnote_seen.clear();
    }
    children(tree, tree.root, out, scratch);
    // The footnotes <section> (referenced definitions, in reference order) follows
    // the document body — matching mdast-util-to-hast's footer.
    if tree.opts.footnotes && !scratch.footnote_order.is_empty() {
        render_footnote_section(tree, out, scratch);
    }
    // External-link transform: a single post-render pass over the finished HTML.
    if tree.opts.external_links {
        decorate_external_links(out);
    }
}

/// Add `rel="nofollow"` to every `<a>` whose href begins with `http://`/`https://`
/// (the rehype-external-links default). Safe as a post-pass because `<` is always
/// escaped to `&lt;` in text/code, so the literal `<a href="` appears only in
/// genuine link tags (inline links, autolinks, and raw-HTML links — which the
/// rehype transform also decorates). `rel` is inserted before the tag's `>`, after
/// any `href`/`title` (matching hast property order); the scan is quote-aware so a
/// `>` inside an attribute value is not mistaken for the tag end.
fn decorate_external_links(out: &mut String) {
    const PAT: &str = "<a href=\"";
    if !out.contains(PAT) {
        return;
    }
    let src = std::mem::take(out);
    let mut rest = src.as_str();
    while let Some(p) = rest.find(PAT) {
        out.push_str(&rest[..p]);
        let tag = &rest[p..]; // starts with `<a href="`
        let href_start = PAT.len();
        let Some(q) = tag[href_start..].find('"') else {
            out.push_str(tag);
            return;
        };
        let href = &tag[href_start..href_start + q];
        // Find the tag-closing `>` (quote-aware), starting past the href value.
        let b = tag.as_bytes();
        let mut j = href_start + q + 1;
        let mut in_quote = false;
        while j < b.len() {
            match b[j] {
                b'"' => in_quote = !in_quote,
                b'>' if !in_quote => break,
                _ => {}
            }
            j += 1;
        }
        out.push_str(&tag[..j]);
        if href.starts_with("http://") || href.starts_with("https://") {
            out.push_str(" rel=\"nofollow\"");
        }
        rest = &tag[j..]; // from the `>` onward
    }
    out.push_str(rest);
}

/// Render the GFM footnotes `<section>` exactly as remark-rehype does: an ordered
/// list of the referenced definitions (in first-reference order), each with one
/// backref per reference appended to its last paragraph (or as a bare anchor when
/// the last block is not a paragraph).
fn render_footnote_section(tree: &Tree, out: &mut String, scratch: &mut Scratch) {
    // Map each referenced identifier to its first definition node (first wins).
    let order: Vec<String> = scratch.footnote_order.clone();
    cr(out);
    out.push_str(
        "<section data-footnotes class=\"footnotes\">\
         <h2 class=\"sr-only\" id=\"footnote-label\">Footnotes</h2>\n<ol>\n",
    );
    for (i, id) in order.iter().enumerate() {
        let num = i + 1;
        let refcount = scratch.footnote_seen.get(id).copied().unwrap_or(1);
        let enc = encode_footnote_id(id);
        let Some(def) = first_footnote_def(tree, id) else {
            continue;
        };
        out.push_str("<li id=\"user-content-fn-");
        out.push_str(&enc);
        out.push_str("\">\n");
        render_footnote_def_body(tree, def, num, refcount, &enc, out, scratch);
        out.push_str("</li>\n");
    }
    out.push_str("</ol>\n</section>");
}

/// The first `FootnoteDef` node whose identifier matches (definitions are matched
/// first-wins, like link reference definitions).
fn first_footnote_def(tree: &Tree, id: &str) -> Option<usize> {
    (0..tree.nodes.len()).find(|&n| {
        tree.nodes[n].kind == Kind::FootnoteDef && tree.fn_def(n).identifier == id
    })
}

/// Append `↩` backref anchor(s) for footnote `num` (one per reference, `1..=count`).
/// `nl` adds a trailing newline after each (used in the bare, non-paragraph case).
fn footnote_backrefs(out: &mut String, enc: &str, num: usize, count: u32, nl: bool) {
    for k in 1..=count {
        out.push_str("<a href=\"#user-content-fnref-");
        out.push_str(enc);
        if k > 1 {
            out.push('-');
            out.push_str(&k.to_string());
        }
        out.push_str("\" data-footnote-backref=\"\" aria-label=\"Back to reference ");
        out.push_str(&num.to_string());
        if k > 1 {
            out.push('-');
            out.push_str(&k.to_string());
        }
        out.push_str("\" class=\"data-footnote-backref\">↩");
        if k > 1 {
            out.push_str("<sup>");
            out.push_str(&k.to_string());
            out.push_str("</sup>");
        }
        out.push_str("</a>");
        if nl {
            out.push('\n');
        } else if k < count {
            out.push(' ');
        }
    }
}

/// Render a footnote definition's blocks into the `<li>`, injecting backrefs into
/// the last paragraph (or appending them as bare anchors after a non-paragraph
/// last block).
fn render_footnote_def_body(
    tree: &Tree,
    def: usize,
    num: usize,
    refcount: u32,
    enc: &str,
    out: &mut String,
    scratch: &mut Scratch,
) {
    let mut c = tree.first_child(def);
    while let Some(ci) = c {
        let is_last = tree.next_sibling(ci).is_none();
        if is_last && tree.nodes[ci].kind == Kind::Paragraph {
            // Backrefs go inside the final paragraph, after a separating space.
            cr(out);
            out.push_str("<p>");
            render_inline(tree.content(ci), out, &tree.refmap, scratch, tree.opts);
            out.push(' ');
            footnote_backrefs(out, enc, num, refcount, false);
            out.push_str("</p>");
            cr(out);
        } else {
            render_node(tree, ci, out, scratch);
            if is_last {
                // Last block is not a paragraph: backrefs follow as bare anchors.
                cr(out);
                footnote_backrefs(out, enc, num, refcount, true);
            }
        }
        c = tree.next_sibling(ci);
    }
}

/// PROTOTYPE built-in transform: derive a github-slugger-style id from a heading's
/// already-rendered inline HTML (strip tags, unescape the four named entities,
/// lowercase, keep `[a-z0-9_-]` + Unicode alphanumerics, spaces→`-`), then
/// de-duplicate with a `-N` suffix. The Rust equivalent of rehype-slug, applied
/// in the render walk so the common "headings get anchors" task never leaves wasm.
fn heading_slug(html: &str, seen: &mut std::collections::HashMap<String, u32>) -> String {
    // 1. strip tags + unescape entities -> plain text
    let mut text = String::with_capacity(html.len());
    let mut rest = html;
    while !rest.is_empty() {
        let b = rest.as_bytes()[0];
        if b == b'<' {
            rest = rest.find('>').map_or("", |p| &rest[p + 1..]);
        } else if b == b'&' {
            if let Some(r) = rest.strip_prefix("&amp;") {
                text.push('&');
                rest = r;
            } else if let Some(r) = rest.strip_prefix("&lt;") {
                text.push('<');
                rest = r;
            } else if let Some(r) = rest.strip_prefix("&gt;") {
                text.push('>');
                rest = r;
            } else if let Some(r) = rest.strip_prefix("&quot;") {
                text.push('"');
                rest = r;
            } else {
                text.push('&');
                rest = &rest[1..];
            }
        } else {
            let c = rest.chars().next().unwrap();
            text.push(c);
            rest = &rest[c.len_utf8()..];
        }
    }
    // 2. slugify
    let mut slug = String::with_capacity(text.len());
    for c in text.chars() {
        if c == ' ' {
            slug.push('-');
        } else if c == '-' || c == '_' {
            slug.push(c);
        } else if c.is_alphanumeric() {
            slug.extend(c.to_lowercase());
        }
    }
    // 3. de-duplicate (first wins as-is; repeats get -1, -2, … like github-slugger)
    match seen.get_mut(&slug) {
        Some(n) => {
            *n += 1;
            format!("{slug}-{n}")
        }
        None => {
            seen.insert(slug.clone(), 0);
            slug
        }
    }
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
            // GFM task list: a list item's first paragraph led by `[ ]`/`[x]`
            // emits a checkbox and drops the marker. Gated — off keeps the exact
            // original path (no slice); on defers to the out-of-line `task_input`.
            let content = tree.content(idx);
            // Cheap inline gate (parent is a list item) keeps the out-of-line
            // `task_input` off the vast majority of paragraphs.
            let content = if Options::GFM
                && tree.opts.tasklist
                && tree.nodes[tree.nodes[idx].parent].kind == Kind::Item
            {
                &content[task_input(tree, idx, out)..]
            } else {
                content
            };
            render_inline(content, out, &tree.refmap, scratch, tree.opts);
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
            if tree.opts.heading_ids {
                // Built-in transform: render the inline once into a scratch buffer,
                // derive the id from it, then reuse the buffer (no double render).
                let mut inner = String::new();
                render_inline(tree.content(idx), &mut inner, &tree.refmap, scratch, tree.opts);
                let slug = heading_slug(&inner, &mut scratch.slugs);
                out.push_str(" id=\"");
                out.push_str(&slug);
                out.push_str("\">");
                out.push_str(&inner);
            } else {
                out.push('>');
                render_inline(tree.content(idx), out, &tree.refmap, scratch, tree.opts);
            }
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
            // Diagram extension hook: when compiled out, `handled` is the literal
            // `false`, so `if !handled { … }` folds to the original arm verbatim.
            #[cfg(feature = "diagram")]
            let handled = tree.opts.diagram && crate::ext::diagram::try_render(tree, idx, out);
            #[cfg(not(feature = "diagram"))]
            let handled = false;
            if !handled {
                out.push_str("<pre><code");
                if let Some(word) = tree.info(idx).split_whitespace().next() {
                    out.push_str(" class=\"language-");
                    escape_html(crate::inline::unescape_string(word).as_ref(), out);
                    out.push('"');
                }
                out.push('>');
                escape_html(tree.content(idx), out);
                out.push_str("</code></pre>");
            }
            cr(out);
        }
        Kind::HtmlBlock => {
            cr(out);
            if Options::GFM && tree.opts.tagfilter {
                filter_html(tree.content(idx), out);
            } else {
                out.push_str(tree.content(idx));
            }
            cr(out);
        }
        // SPIKE (`ast` feature): reference definitions produce no HTML.
        #[cfg(feature = "ast")]
        Kind::Definition => {}
        // Frontmatter produces no HTML output (matching remark-frontmatter).
        Kind::Frontmatter => {}
        // A footnote definition emits nothing where it sits; referenced ones are
        // collected and rendered as the footnotes <section> after the document.
        Kind::FootnoteDef => {}
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
        #[cfg(feature = "gfm")]
        Kind::Table => render_table(tree, idx, out, scratch),
    }
}

#[cfg(feature = "gfm")]
/// Per-column GFM table alignment.
#[derive(Clone, Copy)]
enum Align {
    None,
    Left,
    Center,
    Right,
}

#[cfg(feature = "gfm")]
/// Split a table row into raw cell slices, honoring `\|` escapes and dropping a
/// single optional leading/trailing pipe.
fn split_row_cells(line: &str) -> Vec<&str> {
    let t = line.trim_matches([' ', '\t']);
    let b = t.as_bytes();
    let mut cells = Vec::new();
    let mut start = 0;
    let mut esc = false;
    for (k, &c) in b.iter().enumerate() {
        if esc {
            esc = false;
        } else if c == b'\\' {
            esc = true;
        } else if c == b'|' {
            cells.push(&t[start..k]);
            start = k + 1;
        }
    }
    cells.push(&t[start..]);
    if cells.first().is_some_and(|c| c.is_empty()) {
        cells.remove(0);
    }
    if cells.len() > 1 && cells.last().is_some_and(|c| c.is_empty()) {
        cells.pop();
    }
    cells
}

#[cfg(feature = "gfm")]
/// Parse the delimiter row into per-column alignments.
fn parse_aligns(delim: &str) -> Vec<Align> {
    split_row_cells(delim)
        .iter()
        .map(|c| {
            let c = c.trim();
            match (c.starts_with(':'), c.ends_with(':')) {
                (true, true) => Align::Center,
                (true, false) => Align::Left,
                (false, true) => Align::Right,
                (false, false) => Align::None,
            }
        })
        .collect()
}

#[cfg(feature = "gfm")]
/// Emit one table row's cells as `<th>`/`<td>` (`tag`), padded/truncated to the
/// column count and tagged with alignment.
fn emit_row(
    tree: &Tree,
    row: &str,
    aligns: &[Align],
    tag: &str,
    out: &mut String,
    scratch: &mut Scratch,
) {
    let cells = split_row_cells(row);
    for (col, &align) in aligns.iter().enumerate() {
        let raw = cells.get(col).map_or("", |c| c.trim());
        // GFM unescapes `\|` → `|` at the table layer, before inline parsing —
        // so a pipe inside a code span renders as `|`, not `\|`.
        let cell = if raw.contains("\\|") {
            Cow::Owned(raw.replace("\\|", "|"))
        } else {
            Cow::Borrowed(raw)
        };
        out.push('<');
        out.push_str(tag);
        out.push_str(match align {
            Align::None => "",
            Align::Left => " align=\"left\"",
            Align::Center => " align=\"center\"",
            Align::Right => " align=\"right\"",
        });
        out.push('>');
        render_inline(&cell, out, &tree.refmap, scratch, tree.opts);
        out.push_str("</");
        out.push_str(tag);
        out.push_str(">\n");
    }
}

#[cfg(feature = "gfm")]
/// Render a GFM pipe table. Content is `header\ndelimiter\n[data rows…]`.
fn render_table(tree: &Tree, idx: usize, out: &mut String, scratch: &mut Scratch) {
    let content = tree.content(idx);
    let mut lines = content.lines().filter(|l| !l.trim().is_empty());
    let (Some(header), Some(delim)) = (lines.next(), lines.next()) else {
        return;
    };
    let aligns = parse_aligns(delim);
    cr(out);
    out.push_str("<table>\n<thead>\n<tr>\n");
    emit_row(tree, header, &aligns, "th", out, scratch);
    out.push_str("</tr>\n</thead>\n");
    let mut body_open = false;
    for row in lines {
        if !body_open {
            out.push_str("<tbody>\n");
            body_open = true;
        }
        out.push_str("<tr>\n");
        emit_row(tree, row, &aligns, "td", out, scratch);
        out.push_str("</tr>\n");
    }
    if body_open {
        out.push_str("</tbody>\n");
    }
    out.push_str("</table>");
    cr(out);
}

/// GFM task list: if `para` is the first child of a list item and begins with a
/// `[ ]`/`[x]`/`[X]` marker (followed by whitespace or end), append the disabled
/// checkbox `<input>` and return the byte count to strip from the content;
/// otherwise return 0. Out-of-line and touching no recursive function, so it
/// never perturbs the hot `render_node`'s codegen (a marker-handling branch
/// inlined there cost ~0.5% on the default path).
#[inline(never)]
fn task_input(tree: &Tree, para: usize, out: &mut String) -> usize {
    let item = tree.nodes[para].parent;
    if tree.nodes[item].kind != Kind::Item || tree.first_child(item) != Some(para) {
        return 0;
    }
    let s = tree.content(para).as_bytes();
    if s.len() < 3 || s[0] != b'[' || s[2] != b']' {
        return 0;
    }
    let checked = match s[1] {
        b' ' => false,
        b'x' | b'X' => true,
        _ => return 0,
    };
    if !(s.len() == 3 || matches!(s[3], b' ' | b'\t' | b'\n')) {
        return 0;
    }
    out.push_str(if checked {
        "<input checked=\"\" disabled=\"\" type=\"checkbox\">"
    } else {
        "<input disabled=\"\" type=\"checkbox\">"
    });
    3
}

/// GFM disallowed raw-HTML tags (case-insensitive); a `<` starting one of these
/// is neutralized to `&lt;` (GFM §6.11).
const TAGFILTER_TAGS: [&[u8]; 9] = [
    b"title",
    b"textarea",
    b"style",
    b"xmp",
    b"iframe",
    b"noembed",
    b"noframes",
    b"script",
    b"plaintext",
];

/// Does `rest` (the bytes just past a `<`) start with a blacklisted tag name
/// terminated by a tag delimiter (space/tab/newline/`/`/`>`) or end-of-input?
fn is_filtered_tag(rest: &[u8]) -> bool {
    for tag in TAGFILTER_TAGS {
        if rest.len() >= tag.len() && rest[..tag.len()].eq_ignore_ascii_case(tag) {
            return matches!(
                rest.get(tag.len()),
                None | Some(b' ' | b'\t' | b'\n' | b'\r' | 0x0c | b'>' | b'/')
            );
        }
    }
    false
}

/// GFM tag filter: copy `s` to `out`, replacing the leading `<` of any disallowed
/// raw-HTML tag with `&lt;`. Used for both HTML blocks and inline raw HTML.
pub(crate) fn filter_html(s: &str, out: &mut String) {
    let b = s.as_bytes();
    let mut clean = 0;
    let mut i = 0;
    while let Some(off) = memchr1(&b[i..], b'<') {
        let lt = i + off;
        if is_filtered_tag(&b[lt + 1..]) {
            out.push_str(&s[clean..lt]);
            out.push_str("&lt;");
            clean = lt + 1;
        }
        i = lt + 1;
    }
    out.push_str(&s[clean..]);
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
