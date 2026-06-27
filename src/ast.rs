//! SPIKE (`ast` feature) — owned, nested **mdast** built from sparkdown's parse.
//!
//! Goal: verify sparkdown can emit a tree that plugs into the unified/remark
//! ecosystem (MDAST → HAST → HTML, remark plugins). This builds a real nested
//! mdast — blocks *and* inline nodes (emphasis/strong/link/image/inlineCode/…)
//! with semantics captured at parse time (link urls, code values), not scraped
//! back out of the rendered HTML (which is lossy).
//!
//! The tree is rendered to JSON by `examples/mdast_json.rs` and validated by the
//! Node harness under `harness/` against `mdast-util-assert`,
//! `mdast-util-from-markdown` (shape diff), and `mdast-util-to-hast` +
//! `hast-util-to-html` (round-trip to HTML).
//!
//! Scope: pure CommonMark (the default build). unist `position` is emitted on
//! block nodes (accurate line; column/offset accurate at column 1) and inline
//! nodes (block-granular — accurate per-inline offsets are a documented next
//! step). Enough for position-reading plugins like remark-lint.

use crate::block::{Kind, Tree};
use crate::inline::{
    InlineSink, InlineTok, Scratch, SpanTok, render_inline_to_sink, render_inline_to_tokens,
};

/// An owned mdast node. Field names mirror the mdast spec so a thin serializer
/// (in the example) maps each to `{ type, ... }` verbatim.
#[derive(Debug, Clone)]
pub enum Mdast {
    // --- blocks ---
    Root(Vec<Mdast>),
    Paragraph(Vec<Mdast>),
    Heading {
        depth: u8,
        children: Vec<Mdast>,
    },
    Blockquote(Vec<Mdast>),
    List {
        ordered: bool,
        start: Option<u64>,
        spread: bool,
        children: Vec<Mdast>,
    },
    ListItem {
        spread: bool,
        children: Vec<Mdast>,
    },
    ThematicBreak,
    Code {
        lang: Option<String>,
        meta: Option<String>,
        value: String,
    },
    /// A link reference definition `[label]: url "title"`.
    Definition {
        identifier: String,
        label: String,
        url: String,
        title: Option<String>,
    },
    /// Raw HTML — block (here) or inline (from the inline stream).
    Html(String),
    /// YAML frontmatter (`---`); `value` is the text between the fences.
    #[cfg(feature = "frontmatter")]
    Yaml(String),
    /// TOML frontmatter (`+++`); `value` is the text between the fences.
    #[cfg(feature = "frontmatter")]
    Toml(String),
    /// GFM footnote definition `[^label]: …` — a block container.
    #[cfg(feature = "footnotes")]
    FootnoteDefinition {
        identifier: String,
        label: String,
        children: Vec<Mdast>,
    },
    /// Definition list container (remark-definition-list `defList`).
    #[cfg(feature = "deflist")]
    DefList(Vec<Mdast>),
    /// One term of a definition list (`defListTerm`); children are inline.
    #[cfg(feature = "deflist")]
    DefListTerm(Vec<Mdast>),
    /// One description of a definition list (`defListDescription`). `spread` is
    /// true when loose; children are block content (a wrapping `paragraph`).
    #[cfg(feature = "deflist")]
    DefListDescription {
        spread: bool,
        children: Vec<Mdast>,
    },
    /// remark-directive `containerDirective` (`:::name … :::`) — block children.
    #[cfg(feature = "directives")]
    ContainerDirective {
        name: String,
        attributes: Vec<(String, String)>,
        children: Vec<Mdast>,
    },
    /// remark-directive `leafDirective` (`::name`) — inline children (the label).
    #[cfg(feature = "directives")]
    LeafDirective {
        name: String,
        attributes: Vec<(String, String)>,
        children: Vec<Mdast>,
    },
    /// A container directive's `[label]`: serialized as a `paragraph` carrying
    /// `data: { directiveLabel: true }` (matching remark-directive).
    #[cfg(feature = "directives")]
    DirectiveLabel(Vec<Mdast>),
    // --- inline ---
    Text(String),
    Emphasis(Vec<Mdast>),
    Strong(Vec<Mdast>),
    Delete(Vec<Mdast>),
    InlineCode(String),
    Break,
    Link {
        url: String,
        title: Option<String>,
        children: Vec<Mdast>,
    },
    Image {
        url: String,
        title: Option<String>,
        alt: String,
    },
    LinkReference {
        identifier: String,
        label: String,
        reftype: &'static str,
        children: Vec<Mdast>,
    },
    ImageReference {
        identifier: String,
        label: String,
        reftype: &'static str,
        alt: String,
    },
    /// GFM footnote reference `[^label]` (inline leaf).
    #[cfg(feature = "footnotes")]
    FootnoteReference {
        identifier: String,
        label: String,
    },
    /// remark-directive inline `textDirective` (`:name[label]{attrs}`).
    #[cfg(feature = "directives")]
    TextDirective {
        name: String,
        attributes: Vec<(String, String)>,
        children: Vec<Mdast>,
    },
    /// SPIKE: wraps a block node with its unist `position`. Kept as a wrapper
    /// (rather than a field on every variant) to minimize churn; the serializer
    /// folds it into the inner node's JSON object as `"position": …`.
    Positioned(Pos, Box<Mdast>),
}

/// A unist position: `(line, column, offset)` for start and end. Lines/columns
/// are 1-based, offset is 0-based.
#[derive(Debug, Clone)]
pub struct Pos {
    pub start: (u32, u32, u32),
    pub end: (u32, u32, u32),
}

/// Source position context: maps a source byte offset to a unist point. unist
/// `offset`/`column` are **UTF-16** units (JS string indices), so we precompute a
/// byte→UTF-16 prefix table alongside the line-start table.
struct PosCtx {
    line_off: Vec<usize>, // byte offset of each line start
    u16: Vec<u32>,        // u16[b] = UTF-16 units in src[0..b] (exact at char boundaries)
    src_len: usize,
    src: Vec<u8>,
    enabled: bool, // when false, `wrap` returns bare nodes and no position work is done
    /// When `Some`, the wire serializer runs in *string-pooled* mode: every string
    /// is written into the structure as just its `u32` UTF-16 length (an `Option`
    /// string uses `0xFFFFFFFF` for `None`) and its UTF-8 bytes are appended to this
    /// shared pool in preorder-DFS emit order. The `to_mdast_wire_fast_opts` path
    /// then ships `[u32 poolStart][structure][pool]`. `None` = the inline format.
    pool: Option<std::cell::RefCell<Vec<u8>>>,
}

impl PosCtx {
    fn new(src: &str) -> Self {
        let mut line_off = vec![0usize];
        let mut u16 = Vec::with_capacity(src.len() + 1);
        let mut acc = 0u32;
        for ch in src.chars() {
            for _ in 0..ch.len_utf8() {
                u16.push(acc); // every byte of `ch` maps to the pre-`ch` count
            }
            if ch == '\n' {
                line_off.push(u16.len());
            }
            acc += ch.len_utf16() as u32;
        }
        u16.push(acc); // sentinel for `src_len`
        PosCtx {
            line_off,
            u16,
            src_len: src.len(),
            src: src.as_bytes().to_vec(),
            enabled: true,
            pool: None,
        }
    }

    /// A no-position context: builds NO prefix table and copies NO source, so the
    /// render path can build the owned tree without the UTF-16 table + source copy
    /// (~400 KB of allocations) and without wrapping every node in `Positioned`.
    fn disabled() -> Self {
        PosCtx {
            line_off: Vec::new(),
            u16: Vec::new(),
            src_len: 0,
            src: Vec::new(),
            enabled: false,
            pool: None,
        }
    }

    /// A no-position context whose wire serializer runs in *string-pooled* mode:
    /// strings are written into the structure as a bare `u32` UTF-16 length and
    /// their UTF-8 bytes are accumulated into the shared `pool` in DFS emit order.
    /// Used by `to_mdast_wire_fast_opts`.
    fn pooled() -> Self {
        Self::pooled_with(Vec::new())
    }

    /// Like [`PosCtx::pooled`] but seeds the string pool with a caller-provided
    /// (recycled) `Vec<u8>` instead of allocating a fresh one — used by
    /// [`WireFast`] to reuse the pool buffer across calls. The caller is expected
    /// to have `clear()`ed it; capacity is retained.
    fn pooled_with(pool: Vec<u8>) -> Self {
        PosCtx {
            line_off: Vec::new(),
            u16: Vec::new(),
            src_len: 0,
            src: Vec::new(),
            enabled: false,
            pool: Some(std::cell::RefCell::new(pool)),
        }
    }

    /// Wrap `inner` in its unist `position` when enabled; otherwise return it bare.
    fn wrap(&self, start: usize, end: usize, inner: Mdast) -> Mdast {
        if self.enabled {
            Mdast::Positioned(self.pos(start, end), Box::new(inner))
        } else {
            inner
        }
    }

    /// The block-fallback `bpos` passed to [`build_inline`]: the real position when
    /// enabled, a dummy zero `Pos` when disabled (it is never read on that path, and
    /// the empty prefix tables must not be indexed).
    fn bpos(&self, start: usize, end: usize) -> Pos {
        if self.enabled {
            self.pos(start, end)
        } else {
            Pos {
                start: (0, 0, 0),
                end: (0, 0, 0),
            }
        }
    }

    /// A `(line, column, offset)` point at a byte offset — column/offset in UTF-16.
    fn point(&self, off: usize) -> (u32, u32, u32) {
        let off = off.min(self.src_len);
        let line = self.line_off.partition_point(|&s| s <= off).max(1);
        let off16 = self.u16[off];
        let col = off16 - self.u16[self.line_off[line - 1]] + 1;
        (line as u32, col, off16)
    }

    fn pos(&self, start: usize, end: usize) -> Pos {
        Pos {
            start: self.point(start),
            end: self.point(end),
        }
    }

    /// Drop trailing whitespace (spaces/tabs/line endings) from a byte range end.
    fn rtrim(&self, start: usize, mut end: usize) -> usize {
        if !self.enabled {
            return end;
        }
        while end > start && matches!(self.src[end - 1], b' ' | b'\t' | b'\n' | b'\r') {
            end -= 1;
        }
        end
    }

    /// Advance past trailing spaces/tabs to the end of the line (the newline or
    /// EOF). A definition's tracked end sits right after its title; mdast extends
    /// it to the line end, keeping the trailing spaces.
    fn line_content_end(&self, mut end: usize) -> usize {
        while end < self.src_len && matches!(self.src[end], b' ' | b'\t') {
            end += 1;
        }
        end
    }

    /// Indented-code `position.end`: mdast keeps trailing spaces on the last
    /// content line and even a trailing *spaces-only* line, but drops trailing
    /// *zero-width* (empty) lines and the final newline. Drop newlines and the
    /// empty lines they terminate, stopping at the first line that has content.
    fn rtrim_code_end(&self, start: usize, mut end: usize) -> usize {
        if !self.enabled {
            return end;
        }
        loop {
            // Drop one trailing line ending; stop if there is none.
            if end > start && self.src[end - 1] == b'\n' {
                end -= 1;
                if end > start && self.src[end - 1] == b'\r' {
                    end -= 1;
                }
            } else {
                break;
            }
            // If the line we just exposed is zero-width (its end is the buffer
            // start or another newline), it is an empty line — drop it too.
            if end > start && self.src[end - 1] != b'\n' {
                break;
            }
        }
        end
    }

    /// Drop a single trailing line ending.
    fn rtrim_nl(&self, mut end: usize) -> usize {
        if !self.enabled {
            return end;
        }
        if end > 0 && self.src[end - 1] == b'\n' {
            end -= 1;
            if end > 0 && self.src[end - 1] == b'\r' {
                end -= 1;
            }
        }
        end
    }
}

/// Render an mdast tree to HTML entirely in Rust, **byte-identical** to the
/// unified JS pipeline `mdast-util-to-hast` + `hast-util-to-html` (rehype's
/// `toHast(tree)` → `toHtml(...)`), with the default options (no
/// `allowDangerousHtml`). See `emit_md` for the per-node mapping.
///
/// Note: the caller appends the trailing `\n` that rehype-stringify adds.
pub fn render_mdast(node: &Mdast) -> String {
    let mut out = String::with_capacity(1024);
    render_mdast_into(node, &mut out);
    out
}

/// Render into a caller-owned buffer — lets the caller pre-size it from the
/// source length (one allocation instead of ~log2(N) growth reallocs on a large
/// document).
pub fn render_mdast_into(node: &Mdast, out: &mut String) {
    // `mdast-util-to-hast` resolves link/image references against a map of every
    // `definition` in the tree (first wins, CommonMark-normalized identifier,
    // upper-cased as the key). Build it once per document.
    let mut defs: std::collections::HashMap<&str, (&str, Option<&str>)> =
        std::collections::HashMap::new();
    collect_definitions(node, &mut defs);
    emit(node, out, &defs);
    // rehype-stringify appends a trailing newline to the document.
    out.push('\n');
}

/// Unwrap a [`Mdast::Positioned`] wrapper to its inner node (position carries no
/// render meaning).
#[inline]
fn unwrap_pos(node: &Mdast) -> &Mdast {
    let mut n = node;
    while let Mdast::Positioned(_, inner) = n {
        n = inner;
    }
    n
}

/// Walk the tree collecting every link-reference `definition` (first occurrence
/// per identifier wins — `state.definitionById` in `mdast-util-to-hast`).
fn collect_definitions<'a>(
    node: &'a Mdast,
    defs: &mut std::collections::HashMap<&'a str, (&'a str, Option<&'a str>)>,
) {
    use Mdast::*;
    match unwrap_pos(node) {
        Definition {
            identifier,
            url,
            title,
            ..
        } => {
            defs.entry(identifier.as_str())
                .or_insert((url.as_str(), title.as_deref()));
        }
        Root(c)
        | Paragraph(c)
        | Blockquote(c)
        | Heading { children: c, .. }
        | List { children: c, .. }
        | ListItem { children: c, .. }
        | Emphasis(c)
        | Strong(c)
        | Delete(c)
        | Link { children: c, .. }
        | LinkReference { children: c, .. } => {
            for ch in c {
                collect_definitions(ch, defs);
            }
        }
        #[cfg(feature = "footnotes")]
        FootnoteDefinition { children, .. } => {
            for ch in children {
                collect_definitions(ch, defs);
            }
        }
        _ => {}
    }
}

/// `true` once the node (after unwrapping `Positioned`) maps to a hast `<p>`
/// element — used by list-item rendering to decide tight-paragraph unwrapping.
#[inline]
fn is_paragraph(node: &Mdast) -> bool {
    matches!(unwrap_pos(node), Mdast::Paragraph(_))
}

/// `true` if the node renders to *no* hast output (so it is absent from the
/// hast `results` array). With default options, raw `html` nodes are dropped
/// (`allowDangerousHtml: false`), as are link-reference `definition`s.
fn renders_empty(node: &Mdast) -> bool {
    match unwrap_pos(node) {
        Mdast::Html(_) | Mdast::Definition { .. } => true,
        #[cfg(feature = "frontmatter")]
        Mdast::Yaml(_) | Mdast::Toml(_) => true,
        _ => false,
    }
}

/// Append `s` to `out` escaped as hast-util-to-html escapes **text** content:
/// only `&` → `&#x26;` and `<` → `&#x3C;` (the `['<', '&']` subset, numeric).
fn esc_text(s: &str, out: &mut String) {
    let bytes = s.as_bytes();
    let mut clean = 0;
    for (i, &b) in bytes.iter().enumerate() {
        let rep = match b {
            b'&' => "&#x26;",
            b'<' => "&#x3C;",
            _ => continue,
        };
        out.push_str(&s[clean..i]);
        out.push_str(rep);
        clean = i + 1;
    }
    out.push_str(&s[clean..]);
}

/// Append `s` to `out` escaped as hast-util-to-html escapes a double-quoted
/// **attribute** value: `&` → `&#x26;`, `"` → `&#x22;`, `'` → `&#x27;`,
/// `` ` `` → `&#x60;` (the `['"', '&', "'", "`"]` subset, numeric).
fn esc_attr(s: &str, out: &mut String) {
    let bytes = s.as_bytes();
    let mut clean = 0;
    for (i, &b) in bytes.iter().enumerate() {
        let rep = match b {
            b'&' => "&#x26;",
            b'"' => "&#x22;",
            b'\'' => "&#x27;",
            b'`' => "&#x60;",
            _ => continue,
        };
        out.push_str(&s[clean..i]);
        out.push_str(rep);
        clean = i + 1;
    }
    out.push_str(&s[clean..]);
}

/// `true` if `b` is in `normalizeUri`'s safe ASCII set `[!#$&-;=?-Z_a-z~]`
/// (left verbatim; everything else ASCII is percent-encoded).
#[inline]
fn uri_safe(b: u8) -> bool {
    matches!(b,
        b'!' | b'#'
        | 0x24..=0x3B   // $ % & ' ( ) * + , - . / 0-9 : ;
        | b'='
        | 0x3F..=0x5A   // ? @ A-Z
        | b'_'
        | b'a'..=b'z'
        | b'~')
}

#[inline]
fn utf8_len(b: u8) -> usize {
    if b >= 0xF0 {
        4
    } else if b >= 0xE0 {
        3
    } else if b >= 0xC0 {
        2
    } else {
        1
    }
}

#[inline]
fn push_pct(b: u8, out: &mut String) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    out.push('%');
    out.push(HEX[(b >> 4) as usize] as char);
    out.push(HEX[(b & 0xf) as usize] as char);
}

/// Emit a normalized URL into a double-quoted attribute: `normalizeUri` then the
/// attribute escaper. Fused so the URL is normalized straight into `out` and
/// only the raw `&`/`'` need attribute escaping.
fn emit_uri_attr(url: &str, out: &mut String) {
    // normalizeUri leaves only `&` and `'` from the attribute-escape subset
    // (`"` and backtick are percent-encoded by normalizeUri). Normalize into a
    // scratch via the same buffer is awkward; do it in one pass: normalize, and
    // for the two escapable survivors emit the entity directly.
    let bytes = url.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'%'
            && i + 2 < bytes.len()
            && bytes[i + 1].is_ascii_alphanumeric()
            && bytes[i + 2].is_ascii_alphanumeric()
        {
            out.push('%');
            out.push(bytes[i + 1] as char);
            out.push(bytes[i + 2] as char);
            i += 3;
            continue;
        }
        if b == b'&' {
            out.push_str("&#x26;");
            i += 1;
        } else if b == b'\'' {
            out.push_str("&#x27;");
            i += 1;
        } else if b < 0x80 {
            if uri_safe(b) {
                out.push(b as char);
            } else {
                push_pct(b, out);
            }
            i += 1;
        } else {
            let len = utf8_len(b);
            for &cb in &bytes[i..(i + len).min(bytes.len())] {
                push_pct(cb, out);
            }
            i += len;
        }
    }
}

/// `trim-lines`: strip spaces/tabs at the start of each line except the first,
/// and at the end of each line except the last, then emit as escaped text.
fn emit_text_trimmed(s: &str, out: &mut String) {
    let bytes = s.as_bytes();
    let mut line_start = 0;
    let mut first = true;
    let mut i = 0;
    while i <= bytes.len() {
        // Find end of the current line (at `\n`, `\r\n`, `\r`, or string end).
        let at_end = i == bytes.len();
        let is_break = !at_end && (bytes[i] == b'\n' || bytes[i] == b'\r');
        if at_end || is_break {
            let mut a = line_start;
            let mut b = i;
            if !first {
                while a < b && (bytes[a] == b' ' || bytes[a] == b'\t') {
                    a += 1;
                }
            }
            if !at_end {
                while b > a && (bytes[b - 1] == b' ' || bytes[b - 1] == b'\t') {
                    b -= 1;
                }
            }
            if b > a {
                esc_text(&s[a..b], out);
            }
            if at_end {
                break;
            }
            // Emit the line break verbatim (`\r\n` is two bytes).
            if bytes[i] == b'\r' && i + 1 < bytes.len() && bytes[i + 1] == b'\n' {
                out.push_str("\r\n");
                i += 2;
            } else {
                out.push(bytes[i] as char);
                i += 1;
            }
            line_start = i;
            first = false;
            continue;
        }
        i += 1;
    }
}

type DefMap<'a> = std::collections::HashMap<&'a str, (&'a str, Option<&'a str>)>;

/// `state.wrap(results, loose)` over a slice of block children, emitting `\n`
/// between every rendered (non-empty) child, and additionally at the start and
/// end when `loose`. Dropped nodes (raw html, definitions) are skipped.
fn emit_wrapped(children: &[Mdast], loose: bool, out: &mut String, defs: &DefMap) {
    // `wrap`: a leading `\n` is pushed unconditionally when `loose` (even with no
    // children — an empty blockquote is `<blockquote>\n</blockquote>`).
    if loose {
        out.push('\n');
    }
    let mut any = false;
    for ch in children {
        if renders_empty(ch) {
            continue;
        }
        if any {
            out.push('\n');
        }
        emit(ch, out, defs);
        any = true;
    }
    // The trailing `\n` is only added when at least one node was rendered.
    if loose && any {
        out.push('\n');
    }
}

/// Emit inline children in document order (no wrapping line endings).
fn emit_inline(children: &[Mdast], out: &mut String, defs: &DefMap) {
    for ch in children {
        emit(ch, out, defs);
    }
}

/// `listLoose(list)`: a list is loose if its own `spread` is set, or any item is
/// loose (`item.spread`, or — when unset — `item.children.len() > 1`). Here
/// `spread` is always concrete on our nodes, so it is read directly.
fn list_is_loose(spread: bool, children: &[Mdast]) -> bool {
    if spread {
        return true;
    }
    children
        .iter()
        .any(|c| matches!(unwrap_pos(c), Mdast::ListItem { spread, .. } if *spread))
}

fn emit(node: &Mdast, out: &mut String, defs: &DefMap) {
    use Mdast::*;
    use std::fmt::Write;
    match node {
        Positioned(_, inner) => emit(inner, out, defs),
        // root: wrap(all, false) — `\n` between blocks, none at start/end.
        Root(c) => emit_wrapped(c, false, out, defs),
        Paragraph(c) => {
            out.push_str("<p>");
            emit_inline(c, out, defs);
            out.push_str("</p>");
        }
        Heading { depth, children } => {
            let _ = write!(out, "<h{depth}>");
            emit_inline(children, out, defs);
            let _ = write!(out, "</h{depth}>");
        }
        // blockquote: wrap(all, true).
        Blockquote(c) => {
            out.push_str("<blockquote>");
            emit_wrapped(c, true, out, defs);
            out.push_str("</blockquote>");
        }
        List {
            ordered,
            start,
            spread,
            children,
        } => {
            if *ordered {
                match start {
                    Some(n) if *n != 1 => {
                        let _ = write!(out, "<ol start=\"{n}\">");
                    }
                    _ => out.push_str("<ol>"),
                }
            } else {
                out.push_str("<ul>");
            }
            // list children are always wrapped loose: `\n` around every item.
            let loose = list_is_loose(*spread, children);
            out.push('\n');
            let mut first = true;
            for ch in children {
                if !first {
                    out.push('\n');
                }
                first = false;
                emit_list_item(ch, loose, out, defs);
            }
            out.push('\n');
            out.push_str(if *ordered { "</ol>" } else { "</ul>" });
        }
        // A bare ListItem (not reached via List) — render loosely standalone.
        ListItem { children, .. } => emit_list_item_inner(children, true, out, defs),
        ThematicBreak => out.push_str("<hr>"),
        Code { lang, value, .. } => {
            out.push_str("<pre><code");
            if let Some(l) = lang {
                // CM/GH keep only the first whitespace-delimited token.
                let first = l
                    .split([' ', '\t', '\n', '\r', '\x0c'])
                    .next()
                    .unwrap_or("");
                if !first.is_empty() {
                    out.push_str(" class=\"language-");
                    esc_attr(first, out);
                    out.push('"');
                }
            }
            out.push('>');
            // `value ? value + '\n' : ''` — empty code emits no trailing newline.
            if !value.is_empty() {
                esc_text(value, out);
                out.push('\n');
            }
            out.push_str("</code></pre>");
        }
        // Raw html is dropped with default options (`allowDangerousHtml: false`).
        Html(_) => {}
        Text(s) => emit_text_trimmed(s, out),
        Emphasis(c) => {
            out.push_str("<em>");
            emit_inline(c, out, defs);
            out.push_str("</em>");
        }
        Strong(c) => {
            out.push_str("<strong>");
            emit_inline(c, out, defs);
            out.push_str("</strong>");
        }
        Delete(c) => {
            out.push_str("<del>");
            emit_inline(c, out, defs);
            out.push_str("</del>");
        }
        InlineCode(s) => {
            out.push_str("<code>");
            // inlineCode: newlines collapse to a single space, then text-escape.
            emit_inline_code(s, out);
            out.push_str("</code>");
        }
        Break => out.push_str("<br>\n"),
        Link {
            url,
            title,
            children,
        } => {
            out.push_str("<a href=\"");
            emit_uri_attr(url, out);
            out.push('"');
            if let Some(t) = title {
                out.push_str(" title=\"");
                esc_attr(t, out);
                out.push('"');
            }
            out.push('>');
            emit_inline(children, out, defs);
            out.push_str("</a>");
        }
        Image { url, title, alt } => {
            out.push_str("<img src=\"");
            emit_uri_attr(url, out);
            out.push_str("\" alt=\"");
            esc_attr(alt, out);
            out.push('"');
            if let Some(t) = title {
                out.push_str(" title=\"");
                esc_attr(t, out);
                out.push('"');
            }
            out.push('>');
        }
        LinkReference {
            identifier,
            label,
            reftype,
            children,
        } => match defs.get(identifier.as_str()) {
            Some(&(url, title)) => {
                out.push_str("<a href=\"");
                emit_uri_attr(url, out);
                out.push('"');
                if let Some(t) = title {
                    out.push_str(" title=\"");
                    esc_attr(t, out);
                    out.push('"');
                }
                out.push('>');
                emit_inline(children, out, defs);
                out.push_str("</a>");
            }
            None => revert_link(children, label, identifier, reftype, out, defs),
        },
        ImageReference {
            identifier,
            label,
            reftype,
            alt,
        } => {
            match defs.get(identifier.as_str()) {
                Some(&(url, title)) => {
                    out.push_str("<img src=\"");
                    emit_uri_attr(url, out);
                    out.push_str("\" alt=\"");
                    esc_attr(alt, out);
                    out.push('"');
                    if let Some(t) = title {
                        out.push_str(" title=\"");
                        esc_attr(t, out);
                        out.push('"');
                    }
                    out.push('>');
                }
                None => {
                    // revert: `![alt` + suffix, as one escaped text node.
                    let mut s = String::with_capacity(alt.len() + label.len() + 6);
                    s.push_str("![");
                    s.push_str(alt);
                    push_ref_suffix(&mut s, label, identifier, reftype);
                    esc_text(&s, out);
                }
            }
        }
        // Link reference definitions render nothing.
        Definition { .. } => {}
        // Extension nodes — minimal coverage so the match is exhaustive in every
        // feature build (the CommonMark corpus this spike times never produces them).
        #[cfg(feature = "frontmatter")]
        Yaml(_) | Toml(_) => {}
        #[cfg(feature = "footnotes")]
        FootnoteDefinition { children, .. } => emit_wrapped(children, false, out, defs),
        #[cfg(feature = "footnotes")]
        FootnoteReference { .. } => {}
        #[cfg(feature = "deflist")]
        DefList(c) => {
            out.push_str("<dl>\n");
            for ch in c {
                emit(ch, out, defs);
                out.push('\n');
            }
            out.push_str("</dl>");
        }
        #[cfg(feature = "deflist")]
        DefListTerm(c) => {
            out.push_str("<dt>");
            emit_inline(c, out, defs);
            out.push_str("</dt>");
        }
        #[cfg(feature = "deflist")]
        DefListDescription { children, .. } => {
            out.push_str("<dd>");
            emit_inline(children, out, defs);
            out.push_str("</dd>");
        }
        #[cfg(feature = "directives")]
        ContainerDirective { children, .. }
        | LeafDirective { children, .. }
        | DirectiveLabel(children)
        | TextDirective { children, .. } => emit_inline(children, out, defs),
    }
}

/// Render one list item with the `loose` flag of its parent list, opening the
/// `<li>` (the `\n`-wrapping around items is the caller's job).
fn emit_list_item(node: &Mdast, loose: bool, out: &mut String, defs: &DefMap) {
    match unwrap_pos(node) {
        Mdast::ListItem { children, .. } => emit_list_item_inner(children, loose, out, defs),
        // Defensive: anything else just renders inline.
        other => emit(other, out, defs),
    }
}

/// The `list-item` hast handler: insert a `\n` before each child *except* a
/// tight first-paragraph; unwrap a tight `<p>` child to its inline content; and
/// append a trailing `\n` unless the last child is a tight `<p>`.
fn emit_list_item_inner(children: &[Mdast], loose: bool, out: &mut String, defs: &DefMap) {
    out.push_str("<li>");
    // Build the filtered "results" list (dropped nodes are absent in hast).
    let results: Vec<&Mdast> = children
        .iter()
        .map(unwrap_pos)
        .filter(|c| !renders_empty(c))
        .collect();
    for (index, &child) in results.iter().enumerate() {
        let is_p = is_paragraph(child);
        if loose || index != 0 || !is_p {
            out.push('\n');
        }
        if is_p && !loose {
            if let Mdast::Paragraph(c) = child {
                emit_inline(c, out, defs);
            }
        } else {
            emit(child, out, defs);
        }
    }
    if let Some(&tail) = results.last()
        && (loose || !is_paragraph(tail))
    {
        out.push('\n');
    }
    out.push_str("</li>");
}

/// Append the reference suffix used by `revert`: `]` (shortcut), `][]`
/// (collapsed), or `][label]` (full).
fn push_ref_suffix(out: &mut String, label: &str, identifier: &str, reftype: &str) {
    out.push(']');
    match reftype {
        "collapsed" => out.push_str("[]"),
        "full" => {
            out.push('[');
            out.push_str(if label.is_empty() { identifier } else { label });
            out.push(']');
        }
        _ => {}
    }
}

/// `revert` for an undefined link reference: emit `[` + children + suffix, where
/// the leading `[` fuses into the first text child and the suffix into the last.
fn revert_link(
    children: &[Mdast],
    label: &str,
    identifier: &str,
    reftype: &str,
    out: &mut String,
    defs: &DefMap,
) {
    // The bracket/suffix join into adjacent text nodes (so they share one
    // escaped text run). We model that by emitting a leading `[` text and a
    // trailing-suffix text; escaping is identical whether fused or not.
    let mut lead = String::with_capacity(1);
    lead.push('[');
    esc_text(&lead, out);
    emit_inline(children, out, defs);
    let mut suffix = String::with_capacity(identifier.len() + 2);
    push_ref_suffix(&mut suffix, label, identifier, reftype);
    esc_text(&suffix, out);
}

/// `inlineCode`: collapse CR/LF (and CRLF) to a single space, then text-escape.
fn emit_inline_code(s: &str, out: &mut String) {
    let bytes = s.as_bytes();
    let mut clean = 0;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\n' || b == b'\r' {
            esc_text(&s[clean..i], out);
            out.push(' ');
            if b == b'\r' && i + 1 < bytes.len() && bytes[i + 1] == b'\n' {
                i += 2;
            } else {
                i += 1;
            }
            clean = i;
            continue;
        }
        i += 1;
    }
    esc_text(&s[clean..], out);
}

/// Parse `src` and build the nested mdast tree (CommonMark), with accurate unist
/// `position` (UTF-16 line/column/offset) on every node.
pub fn to_mdast(src: &str) -> Mdast {
    to_mdast_opts(src, crate::Options::default())
}

/// Like [`to_mdast`] but with opt-in grammar extensions (e.g. frontmatter).
pub fn to_mdast_opts(src: &str, opts: crate::Options) -> Mdast {
    let tree = crate::block::parse_with_opts(src, opts);
    let mut scratch = fn_scratch(&tree);
    let ctx = PosCtx::new(src);
    #[cfg_attr(not(feature = "gfm"), allow(unused_mut))]
    let mut root = block(&tree, tree.root, &mut scratch, &ctx).0;
    #[cfg(feature = "gfm")]
    if opts.autolink {
        al_transform_tree(&mut root);
    }
    root
}

/// Like [`to_mdast_opts`] but builds the tree WITHOUT unist `position` — for the
/// HTML-render path, which never reads it. Skips the UTF-16 prefix-table build and
/// the source copy in `PosCtx`, and emits bare nodes instead of wrapping every
/// one in `Mdast::Positioned`. The resulting tree renders byte-identically; only
/// the `position` metadata (unused by the renderer) is absent.
pub fn to_mdast_opts_nopos(src: &str, opts: crate::Options) -> Mdast {
    let tree = crate::block::parse_with_opts(src, opts);
    let mut scratch = fn_scratch(&tree);
    let ctx = PosCtx::disabled();
    #[cfg_attr(not(feature = "gfm"), allow(unused_mut))]
    let mut root = block(&tree, tree.root, &mut scratch, &ctx).0;
    #[cfg(feature = "gfm")]
    if opts.autolink {
        al_transform_tree(&mut root);
    }
    root
}

/// GFM autolink-literal transform over a built mdast tree — the object-path
/// counterpart of [`WireSink`]'s streaming `text` handling. Replaces phrasing
/// `text` nodes with the transform's pieces (bare, i.e. position-less, matching
/// `mdast-util-find-and-replace`), recursing into every container except
/// `link`/`linkReference`/`image` subtrees (`ignore: ['link','linkReference']`,
/// and `image` has no phrasing children).
#[cfg(feature = "gfm")]
fn al_transform_tree(node: &mut Mdast) {
    use Mdast::*;
    let inner = match node {
        Positioned(_, b) => b.as_mut(),
        n => n,
    };
    let children: &mut Vec<Mdast> = match inner {
        Link { .. } | Image { .. } | LinkReference { .. } => return,
        Root(c)
        | Paragraph(c)
        | Blockquote(c)
        | Heading { children: c, .. }
        | List { children: c, .. }
        | ListItem { children: c, .. }
        | Emphasis(c)
        | Strong(c)
        | Delete(c) => c,
        #[cfg(feature = "footnotes")]
        FootnoteDefinition { children, .. } => children,
        // Non-GFM container extensions also carry phrasing text that the wire
        // path transforms; recurse so the object path stays consistent with it
        // (and with remark) when those features are enabled.
        #[cfg(feature = "deflist")]
        DefList(c) | DefListTerm(c) => c,
        #[cfg(feature = "deflist")]
        DefListDescription { children, .. } => children,
        #[cfg(feature = "directives")]
        ContainerDirective { children, .. }
        | LeafDirective { children, .. }
        | TextDirective { children, .. } => children,
        #[cfg(feature = "directives")]
        DirectiveLabel(c) => c,
        _ => return,
    };
    let mut i = 0;
    while i < children.len() {
        let val = match &children[i] {
            Text(s) => Some(s.as_str()),
            Positioned(_, b) => match b.as_ref() {
                Text(s) => Some(s.as_str()),
                _ => None,
            },
            _ => None,
        };
        if let Some(pieces) = val.and_then(crate::inline::al_transform_text) {
            let repl: Vec<Mdast> = pieces
                .into_iter()
                .map(|p| match p {
                    crate::inline::AlPiece::Text(t) => Text(t),
                    crate::inline::AlPiece::Link { url, text } => Link {
                        url,
                        title: None,
                        children: vec![Text(text)],
                    },
                })
                .collect();
            let n = repl.len();
            children.splice(i..=i, repl);
            i += n;
        } else {
            i += 1;
        }
    }
    for c in children.iter_mut() {
        al_transform_tree(c);
    }
}

/// A fresh [`Scratch`] seeded with the tree's footnote labels, so inline
/// `[^label]` references resolve while building the mdast (forward refs work
/// because the label set is collected in the block pass).
fn fn_scratch(
    #[cfg_attr(not(feature = "footnotes"), allow(unused_variables))] tree: &Tree,
) -> Scratch {
    #[cfg_attr(not(feature = "footnotes"), allow(unused_mut))]
    let mut scratch = Scratch::new();
    #[cfg(feature = "footnotes")]
    if tree.opts.footnotes {
        scratch.footnote_ids.clone_from(&tree.footnote_ids);
    }
    scratch
}

/// The mdast `value` of a frontmatter node: the raw text between the fences with
/// exactly one trailing line ending dropped. Internal line endings are kept
/// verbatim — remark-frontmatter does not normalize CRLF inside the value.
#[cfg(feature = "frontmatter")]
fn frontmatter_value(raw: &str) -> &str {
    let raw = raw.strip_suffix('\n').unwrap_or(raw);
    raw.strip_suffix('\r').unwrap_or(raw)
}

/// Build a block node and return it plus its source byte end (for parent
/// containers, whose end is their last child's end).
fn block(tree: &Tree, idx: usize, scratch: &mut Scratch, ctx: &PosCtx) -> (Mdast, usize) {
    let node = &tree.nodes[idx];
    let (s, e) = tree.src_span(idx);
    let (sb, se) = (s as usize, e as usize);
    let (inner, start_b, end_b) = match node.kind {
        Kind::Document => {
            let (kids, _) = block_children(tree, idx, scratch, ctx);
            (Mdast::Root(kids), 0, ctx.src_len)
        }
        Kind::Paragraph => {
            // mdast keeps trailing spaces on the last line (only the newline is
            // dropped); the text node's value trims them separately.
            let eb = ctx.rtrim_nl(se);
            (
                Mdast::Paragraph(inline(tree, idx, scratch, ctx, sb, eb)),
                sb,
                eb,
            )
        }
        Kind::Heading => {
            // atx/setext span the whole line(s); the text child is trimmed.
            let eb = ctx.rtrim_nl(se);
            let depth = node.level;
            (
                Mdast::Heading {
                    depth,
                    children: inline(tree, idx, scratch, ctx, sb, eb),
                },
                sb,
                eb,
            )
        }
        Kind::BlockQuote => {
            // A blockquote ends at its last `>`-marked line (tracked as `se`),
            // which extends past the last child for trailing blank `>` lines.
            let (kids, last) = block_children(tree, idx, scratch, ctx);
            let end = last.map_or(se, |l| l.max(se));
            (Mdast::Blockquote(kids), sb, end)
        }
        Kind::ThematicBreak => (Mdast::ThematicBreak, sb, ctx.rtrim_nl(se)),
        Kind::CodeBlock => {
            let (lang, meta) = code_info(tree.info(idx));
            let mut value = tree.content(idx).to_owned();
            if value.ends_with('\n') {
                value.pop();
            }
            // Fenced blocks span their full source (incl. the closing fence, or
            // the trailing newline of an unclosed block); indented blocks trim
            // trailing blank lines.
            let eb = if node.fenced {
                se
            } else {
                ctx.rtrim_code_end(sb, se)
            };
            (Mdast::Code { lang, meta, value }, sb, eb)
        }
        Kind::HtmlBlock => {
            // The position end matches where the `value` ends (type-1-at-EOF
            // keeps its trailing newline; others drop one line ending) — not the
            // node's raw `src_end`, which may run past trailing blank lines.
            (
                Mdast::Html(tree.html_value(idx).to_owned()),
                sb,
                tree.html_ast_end(idx) as usize,
            )
        }
        // The `Frontmatter` variant is always compiled (to keep the hot block
        // matches stable), but a node only exists with the `frontmatter` feature.
        #[cfg(not(feature = "frontmatter"))]
        Kind::Frontmatter => unreachable!("Frontmatter node requires the `frontmatter` feature"),
        #[cfg(feature = "frontmatter")]
        Kind::Frontmatter => {
            // `level` 1 = TOML (`+++`), 0 = YAML (`---`). The span is already the
            // exact mdast range: start at offset 0, end at the closing fence's
            // content end (trailing spaces kept, newline dropped — set in parse).
            let value = frontmatter_value(tree.content(idx)).to_owned();
            let inner = if node.level == 1 {
                Mdast::Toml(value)
            } else {
                Mdast::Yaml(value)
            };
            (inner, sb, se)
        }
        // The `FootnoteDef` variant is always compiled (to keep the hot block
        // matches stable), but a node only exists with the `footnotes` feature.
        #[cfg(not(feature = "footnotes"))]
        Kind::FootnoteDef => unreachable!("FootnoteDef node requires the `footnotes` feature"),
        #[cfg(feature = "footnotes")]
        Kind::FootnoteDef => {
            let d = tree.fn_def(idx);
            let (identifier, label) = (d.identifier.clone(), d.label.clone());
            let (kids, last) = block_children(tree, idx, scratch, ctx);
            (
                Mdast::FootnoteDefinition {
                    identifier,
                    label,
                    children: kids,
                },
                sb,
                last.unwrap_or(se),
            )
        }
        Kind::Definition => {
            let d = tree.definition(idx);
            (
                Mdast::Definition {
                    identifier: d.identifier.clone(),
                    label: d.label.clone(),
                    url: d.url.clone(),
                    title: d.title.clone(),
                },
                sb,
                // Back up to the last significant char (the span may run past the
                // line for buffered defs), then forward over trailing spaces to
                // the line end — mdast keeps a definition's trailing spaces.
                ctx.line_content_end(ctx.rtrim(sb, se)),
            )
        }
        Kind::List => {
            let ld = node.list.as_ref().unwrap();
            let (kids, last) = block_children(tree, idx, scratch, ctx);
            let m = Mdast::List {
                ordered: ld.ordered,
                start: ld.ordered.then_some(ld.start),
                spread: ld.spread,
                children: kids,
            };
            // `se` (set only for a blockquote-marker blank line absorbed by the
            // list) can extend past the last item; otherwise it is 0.
            (m, sb, last.map_or(se, |l| l.max(se)))
        }
        Kind::Item => {
            let (kids, last) = block_children(tree, idx, scratch, ctx);
            (
                Mdast::ListItem {
                    spread: node.item_spread,
                    children: kids,
                },
                sb,
                last.unwrap_or(se),
            )
        }
        // The deflist variants stay compiled (to keep the hot block match
        // stable), but a node only exists with the `deflist` feature.
        #[cfg(not(feature = "deflist"))]
        Kind::DefList | Kind::DefTerm | Kind::DefDesc => {
            unreachable!("DefList nodes require the `deflist` feature")
        }
        #[cfg(feature = "deflist")]
        Kind::DefList => {
            // Build the custom remark-definition-list shape: each term line is a
            // `defListTerm`, each `: …` line a `defListDescription` wrapping a
            // `paragraph`. Deflist nodes are not gated against from-markdown
            // (it has no deflist grammar), so positions are best-effort.
            let mut kids: Vec<Mdast> = Vec::new();
            let mut last = se;
            let mut c = tree.first_child(idx);
            while let Some(ci) = c {
                match tree.nodes[ci].kind {
                    Kind::DefTerm => {
                        let content = tree.content(ci);
                        let mut co = 0usize;
                        for line in content.split_inclusive('\n') {
                            let lead = line.len() - line.trim_start_matches([' ', '\t']).len();
                            let body = line.trim_matches([' ', '\t', '\n', '\r']);
                            if !body.is_empty() {
                                let off = (co + lead) as u32;
                                last = tree.content_to_src(ci, off + body.len() as u32) as usize;
                                let (pos, inl) = inline_span(tree, ci, off, body, scratch, ctx);
                                let term = Mdast::DefListTerm(inl);
                                kids.push(if ctx.enabled {
                                    Mdast::Positioned(pos, Box::new(term))
                                } else {
                                    term
                                });
                            }
                            co += line.len();
                        }
                    }
                    Kind::DefDesc => {
                        let spread = tree.nodes[ci].level == 1;
                        let content = tree.content(ci);
                        let lead =
                            content.len() - content.trim_start_matches([' ', '\t', '\n']).len();
                        let body = content.trim_matches([' ', '\t', '\n', '\r']);
                        last = tree.content_to_src(ci, (lead + body.len()) as u32) as usize;
                        let (pos, inl) = inline_span(tree, ci, lead as u32, body, scratch, ctx);
                        let para = if ctx.enabled {
                            Mdast::Positioned(pos.clone(), Box::new(Mdast::Paragraph(inl)))
                        } else {
                            Mdast::Paragraph(inl)
                        };
                        let desc = Mdast::DefListDescription {
                            spread,
                            children: vec![para],
                        };
                        kids.push(if ctx.enabled {
                            Mdast::Positioned(pos, Box::new(desc))
                        } else {
                            desc
                        });
                    }
                    _ => {
                        let (m, eb) = block(tree, ci, scratch, ctx);
                        last = eb;
                        kids.push(m);
                    }
                }
                c = tree.next_sibling(ci);
            }
            (Mdast::DefList(kids), sb, last)
        }
        // Term/description nodes are built inline by their `DefList` parent; these
        // arms keep the match exhaustive and are not reached in practice.
        #[cfg(feature = "deflist")]
        Kind::DefTerm => {
            let eb = ctx.rtrim_nl(se);
            (
                Mdast::DefListTerm(inline(tree, idx, scratch, ctx, sb, eb)),
                sb,
                eb,
            )
        }
        #[cfg(feature = "deflist")]
        Kind::DefDesc => {
            let eb = ctx.rtrim_nl(se);
            let para = ctx.wrap(
                sb,
                eb,
                Mdast::Paragraph(inline(tree, idx, scratch, ctx, sb, eb)),
            );
            (
                Mdast::DefListDescription {
                    spread: node.level == 1,
                    children: vec![para],
                },
                sb,
                eb,
            )
        }
        // The directive variants stay compiled (to keep the hot block match
        // stable), but a node only exists with the `directives` feature.
        #[cfg(not(feature = "directives"))]
        Kind::LeafDirective | Kind::ContainerDirective => {
            unreachable!("directive nodes require the `directives` feature")
        }
        #[cfg(feature = "directives")]
        Kind::LeafDirective => {
            let d = tree.directive(idx);
            let eb = ctx.rtrim_nl(se);
            let children = directive_label_children(tree, d, scratch, ctx);
            (
                Mdast::LeafDirective {
                    name: d.name.clone(),
                    attributes: d.attrs.clone(),
                    children,
                },
                sb,
                eb,
            )
        }
        #[cfg(feature = "directives")]
        Kind::ContainerDirective => {
            let d = tree.directive(idx);
            // A `[label]` on the opener becomes a leading `paragraph` carrying
            // `data.directiveLabel` (matching remark-directive); the fenced body
            // parses as ordinary block children.
            let mut kids: Vec<Mdast> = Vec::new();
            if let Some((ls, le)) = d.label {
                let label = directive_label_children(tree, d, scratch, ctx);
                // remark-directive spans the label paragraph over the brackets.
                kids.push(ctx.wrap(
                    ls as usize - 1,
                    le as usize + 1,
                    Mdast::DirectiveLabel(label),
                ));
            }
            let (body, last) = block_children(tree, idx, scratch, ctx);
            kids.extend(body);
            (
                Mdast::ContainerDirective {
                    name: d.name.clone(),
                    attributes: d.attrs.clone(),
                    children: kids,
                },
                sb,
                last.map_or(se, |l| l.max(se)),
            )
        }
        #[cfg(feature = "gfm")]
        Kind::Table => (
            Mdast::Html(tree.content(idx).to_owned()),
            sb,
            ctx.rtrim(sb, se),
        ),
    };
    (ctx.wrap(start_b, end_b, inner), end_b)
}

/// Build the inline children of a block directive's `[label]` (empty when absent).
/// The label is a contiguous source range, so child token offsets map directly.
#[cfg(feature = "directives")]
fn directive_label_children(
    tree: &Tree,
    d: &crate::block::DirData,
    scratch: &mut Scratch,
    ctx: &PosCtx,
) -> Vec<Mdast> {
    let Some((ls, le)) = d.label else {
        return Vec::new();
    };
    let body = tree.source_range(ls, le);
    let toks = render_inline_to_tokens(body, &tree.refmap, scratch, tree.opts);
    let bpos = ctx.bpos(ls as usize, le as usize);
    build_inline(
        body,
        toks,
        tree,
        scratch,
        ctx,
        &|o| ls as usize + o as usize,
        &bpos,
    )
}

/// Tokenize and position-map a single inline run `body` that starts at content
/// offset `content_off` within node `ci`. Used to build definition-list term and
/// description children (which split or trim a block's content). Returns the run's
/// position and its inline mdast children.
fn inline_span(
    tree: &Tree,
    ci: usize,
    content_off: u32,
    body: &str,
    scratch: &mut Scratch,
    ctx: &PosCtx,
) -> (Pos, Vec<Mdast>) {
    let sbb = tree.content_to_src(ci, content_off) as usize;
    let ebb = tree.content_to_src(ci, content_off + body.len() as u32) as usize;
    let toks = render_inline_to_tokens(body, &tree.refmap, scratch, tree.opts);
    let bpos = ctx.bpos(sbb, ebb);
    let inl = build_inline(
        body,
        toks,
        tree,
        scratch,
        ctx,
        &|o| tree.content_to_src(ci, content_off + o) as usize,
        &bpos,
    );
    (bpos, inl)
}

/// Build a container's children; also report the last child's source byte end.
fn block_children(
    tree: &Tree,
    idx: usize,
    scratch: &mut Scratch,
    ctx: &PosCtx,
) -> (Vec<Mdast>, Option<usize>) {
    let mut v = Vec::new();
    let mut last = None;
    let mut c = tree.first_child(idx);
    while let Some(ci) = c {
        let (m, eb) = block(tree, ci, scratch, ctx);
        v.push(m);
        last = Some(eb);
        c = tree.next_sibling(ci);
    }
    (v, last)
}

/// Build a text-bearing block's inline children with per-node source positions.
/// `[sb, eb)` is the block's source span — the fallback when a token's own span
/// is unknown or the content is buffered (no direct source map).
fn inline(
    tree: &Tree,
    idx: usize,
    scratch: &mut Scratch,
    ctx: &PosCtx,
    sb: usize,
    eb: usize,
) -> Vec<Mdast> {
    let toks = render_inline_to_tokens(tree.content(idx), &tree.refmap, scratch, tree.opts);
    let bpos = ctx.bpos(sb, eb);
    let map = |off: u32| tree.content_to_src(idx, off) as usize;
    build_inline(tree.content(idx), toks, tree, scratch, ctx, &map, &bpos)
}

/// An open inline container awaiting its close.
enum Frame {
    Container(&'static str), // "emphasis" | "strong" | "delete"
    Link {
        url: String,
        title: Option<String>,
    },
    LinkRef {
        identifier: String,
        label: String,
        reftype: &'static str,
    },
}

/// Fold the [`SpanTok`] stream into a nested, positioned mdast. `base` is the
/// source byte offset of the inline content (so a content offset `o` maps to
/// source `base + o`); when `None` (buffered content) or a token span is unset,
/// the block-granular `bpos` is used.
fn build_inline(
    // `content`/`tree`/`scratch` are consumed only by the `TextDirective` arm
    // (it re-tokenizes a directive `[label]`), which is cfg'd out without the
    // `directives` feature — so they are threaded but unread in that build.
    #[cfg_attr(not(feature = "directives"), allow(unused_variables))] content: &str,
    toks: Vec<SpanTok>,
    #[cfg_attr(not(feature = "directives"), allow(unused_variables))] tree: &Tree,
    #[cfg_attr(not(feature = "directives"), allow(unused_variables))] scratch: &mut Scratch,
    ctx: &PosCtx,
    map: &dyn Fn(u32) -> usize,
    bpos: &Pos,
) -> Vec<Mdast> {
    let mkpos = |s: u32, e: u32| -> Pos {
        if s == u32::MAX {
            bpos.clone()
        } else {
            // Map the END via the last contained byte (`e-1`) so a span ending
            // exactly at a buf→source segment boundary stays in its own segment.
            let send = if e == 0 { map(0) } else { map(e - 1) + 1 };
            ctx.pos(map(s), send)
        }
    };
    // Wrap an inline node in its `Positioned` (computing the span via `mkpos`) when
    // enabled; otherwise return it bare and skip the position work entirely.
    let wrapm = |s: u32, e: u32, inner: Mdast| -> Mdast {
        if ctx.enabled {
            Mdast::Positioned(mkpos(s, e), Box::new(inner))
        } else {
            inner
        }
    };
    // (frame, children, open_start, open_end).
    let mut stack: Vec<(Option<Frame>, Vec<Mdast>, u32, u32)> =
        vec![(None, Vec::new(), u32::MAX, 0)];
    for SpanTok { tok, start, end } in toks {
        match tok {
            InlineTok::Text(s) => {
                let sib = &mut stack.last_mut().unwrap().1;
                // Coalesce adjacent text (matching from-markdown's single run),
                // extending the merged node's end.
                if ctx.enabled {
                    let pos = mkpos(start, end);
                    if let Some(Mdast::Positioned(ppos, last)) = sib.last_mut()
                        && let Mdast::Text(prev) = last.as_mut()
                    {
                        prev.push_str(&s);
                        ppos.end = pos.end;
                        continue;
                    }
                    sib.push(Mdast::Positioned(pos, Box::new(Mdast::Text(s))));
                } else {
                    if let Some(Mdast::Text(prev)) = sib.last_mut() {
                        prev.push_str(&s);
                        continue;
                    }
                    sib.push(Mdast::Text(s));
                }
            }
            InlineTok::Code(v) => {
                stack
                    .last_mut()
                    .unwrap()
                    .1
                    .push(wrapm(start, end, Mdast::InlineCode(v)));
            }
            InlineTok::Html(h) => {
                stack
                    .last_mut()
                    .unwrap()
                    .1
                    .push(wrapm(start, end, Mdast::Html(h)));
            }
            InlineTok::Break => {
                stack
                    .last_mut()
                    .unwrap()
                    .1
                    .push(wrapm(start, end, Mdast::Break));
            }
            InlineTok::Image { url, title, alt } => {
                stack.last_mut().unwrap().1.push(wrapm(
                    start,
                    end,
                    Mdast::Image { url, title, alt },
                ));
            }
            InlineTok::ImageRef {
                identifier,
                label,
                reftype,
                alt,
            } => {
                stack.last_mut().unwrap().1.push(wrapm(
                    start,
                    end,
                    Mdast::ImageReference {
                        identifier,
                        label,
                        reftype,
                        alt,
                    },
                ));
            }
            #[cfg(feature = "footnotes")]
            InlineTok::FootnoteRef { identifier, label } => {
                stack.last_mut().unwrap().1.push(wrapm(
                    start,
                    end,
                    Mdast::FootnoteReference { identifier, label },
                ));
            }
            #[cfg(feature = "directives")]
            InlineTok::TextDirective { name, attrs, label } => {
                let children = match label {
                    Some((ls, le)) => {
                        let body = &content[ls as usize..le as usize];
                        let toks2 = render_inline_to_tokens(body, &tree.refmap, scratch, tree.opts);
                        // `lpos` is only read (as a fallback `bpos`) on the
                        // position-on path; skip computing it when disabled so the
                        // empty prefix tables are never indexed.
                        let lpos = if ctx.enabled {
                            ctx.pos(map(ls), if le == 0 { map(0) } else { map(le - 1) + 1 })
                        } else {
                            bpos.clone()
                        };
                        build_inline(body, toks2, tree, scratch, ctx, &|o| map(ls + o), &lpos)
                    }
                    None => Vec::new(),
                };
                stack.last_mut().unwrap().1.push(wrapm(
                    start,
                    end,
                    Mdast::TextDirective {
                        name,
                        attributes: attrs,
                        children,
                    },
                ));
            }
            InlineTok::Autolink { url, text } => {
                // The child text spans only the visible text. A `<url>`/`<email>`
                // autolink wraps it in `<>` (one byte each side); a bare GFM
                // autolink has none. Derive the symmetric padding from the span vs
                // the text length so both land right (mirrors `WireSink::autolink`).
                let pad = (((end - start) as usize).saturating_sub(text.len()) / 2) as u32;
                let child = wrapm(
                    start.saturating_add(pad),
                    end.saturating_sub(pad),
                    Mdast::Text(text),
                );
                stack.last_mut().unwrap().1.push(wrapm(
                    start,
                    end,
                    Mdast::Link {
                        url,
                        title: None,
                        children: vec![child],
                    },
                ));
            }
            InlineTok::Open(kind) => {
                stack.push((Some(Frame::Container(kind)), Vec::new(), start, end))
            }
            InlineTok::LinkOpen { url, title } => {
                stack.push((Some(Frame::Link { url, title }), Vec::new(), start, end))
            }
            InlineTok::LinkRefOpen {
                identifier,
                label,
                reftype,
            } => stack.push((
                Some(Frame::LinkRef {
                    identifier,
                    label,
                    reftype,
                }),
                Vec::new(),
                start,
                end,
            )),
            InlineTok::Close(_) | InlineTok::LinkClose => {
                let (frame, children, os, oe) = stack.pop().unwrap();
                let node = match frame {
                    Some(Frame::Container("strong")) => Mdast::Strong(children),
                    Some(Frame::Container("delete")) => Mdast::Delete(children),
                    Some(Frame::Container(_)) => Mdast::Emphasis(children),
                    Some(Frame::Link { url, title }) => Mdast::Link {
                        url,
                        title,
                        children,
                    },
                    Some(Frame::LinkRef {
                        identifier,
                        label,
                        reftype,
                    }) => Mdast::LinkReference {
                        identifier,
                        label,
                        reftype,
                        children,
                    },
                    None => {
                        for c in children {
                            stack.last_mut().unwrap().1.push(c);
                        }
                        continue;
                    }
                };
                // Container span: opener start … max(opener end, closer end). The
                // link opener already carries the whole link's end; emphasis
                // opener carries only its marker, so the closer extends it.
                let cend = if end > oe { end } else { oe };
                stack.last_mut().unwrap().1.push(wrapm(os, cend, node));
            }
        }
    }
    while stack.len() > 1 {
        let (_, children, _, _) = stack.pop().unwrap();
        for c in children {
            stack.last_mut().unwrap().1.push(c);
        }
    }
    stack.pop().unwrap().1
}
/// Split a fenced code-block info string into mdast `(lang, meta)`.
fn code_info(info: &str) -> (Option<String>, Option<String>) {
    let info = crate::inline::unescape_string(info);
    let trimmed = info.trim_start();
    if trimmed.is_empty() {
        return (None, None);
    }
    match trimmed.split_once(char::is_whitespace) {
        Some((lang, rest)) => {
            let meta = rest.trim();
            (
                Some(lang.to_owned()),
                (!meta.is_empty()).then(|| meta.to_owned()),
            )
        }
        None => (Some(trimmed.to_owned()), None),
    }
}

// ---- JSON serialization (zero-dep, for the wasm boundary) ----------------

/// SPIKE: parse `src` and serialize its mdast to a JSON string. Zero-dependency
/// (hand-rolled) so it works in the `wasm32-unknown-unknown` lib build. This is
/// what crosses the wasm→JS boundary in the boundary spike.
pub fn to_mdast_json(src: &str) -> String {
    to_mdast_json_opts(src, crate::Options::default())
}

/// Like [`to_mdast_json`] but with opt-in grammar extensions (e.g. frontmatter).
pub fn to_mdast_json_opts(src: &str, opts: crate::Options) -> String {
    let tree = to_mdast_opts(src, opts);
    // mdast JSON is roughly 3–5× the source; reserve generously to avoid regrows.
    let mut out = String::with_capacity(src.len() * 4 + 64);
    write_json(&tree, &mut out);
    out
}

/// SPIKE (route A): serialize the mdast to a compact little-endian **binary wire
/// format**, read directly out of wasm linear memory into plain JS objects with
/// no JSON string in between. This removes the JSON serialize (Rust) + whole-
/// buffer UTF-8 decode + `JSON.parse` (JS) tax that the `to_mdast_json` boundary
/// pays — the JS reader walks the bytes and builds the same remark-shaped tree.
///
/// Layout, preorder DFS, every value little-endian:
///   per node: `u8 tag`, then position `6×u32` (start line,col,offset; end
///   line,col,offset), then the tag's payload. Container payloads end with
///   `u32 childCount` followed by that many child nodes. Strings: `u32 len` then
///   `len` UTF-8 bytes; an optional string uses `len == 0xFFFF_FFFF` for `None`.
pub fn to_mdast_wire(src: &str) -> Vec<u8> {
    to_mdast_wire_opts(src, crate::Options::default())
}

/// Like [`to_mdast_wire`] but with opt-in grammar extensions (e.g. frontmatter).
pub fn to_mdast_wire_opts(src: &str, opts: crate::Options) -> Vec<u8> {
    // Emit wire bytes *directly* during the parse walk — no intermediate owned
    // `Mdast` tree (its construction was ~0.95 ms / the dominant boundary cost).
    let tree = crate::block::parse_with_opts(src, opts);
    let mut scratch = fn_scratch(&tree);
    let ctx = PosCtx::new(src);
    let mut out = Vec::<u8>::with_capacity(src.len() * 2 + 64);
    bwire(&tree, tree.root, &mut scratch, &ctx, &mut out);
    out
}

/// Like [`to_mdast_wire_opts`] but **without unist `position`**: every node's
/// `6×u32` start/end point is omitted, and the heavy `PosCtx` (UTF-16 prefix
/// table + a full source copy, ~400 KB of allocations) plus the two `point()`
/// lookups per node are skipped entirely. Many remark consumers never read
/// position; this ships ~100 KB less and parses faster. The non-position payload
/// bytes (tags, child counts, strings, reftypes, …) are byte-identical to the
/// position-on wire — the JS reader skips the point reads when told there is no
/// position. The position-on [`to_mdast_wire`] / [`to_mdast_wire_opts`] output is
/// unaffected (the `if ctx.enabled` guards leave the enabled path's bytes and
/// emit order exactly as before).
pub fn to_mdast_wire_nopos_opts(src: &str, opts: crate::Options) -> Vec<u8> {
    let tree = crate::block::parse_with_opts(src, opts);
    let mut scratch = fn_scratch(&tree);
    let ctx = PosCtx::disabled();
    let mut out = Vec::<u8>::with_capacity(src.len() * 2 + 64);
    bwire(&tree, tree.root, &mut scratch, &ctx, &mut out);
    out
}

/// The fastest md→mdast boundary format: a no-position wire whose strings are
/// *string-pooled* into one contiguous block so a JS reader decodes the whole
/// pool with a single `TextDecoder.decode` (then slices substrings) instead of
/// thousands of per-string decodes.
///
/// Returned layout: `[u32 poolStart][structure bytes][pool bytes]`, where
/// `poolStart = 4 + structure_len` is the byte offset (from the start of the
/// returned Vec) at which the pool begins.
///
/// The `structure` is the SAME no-position node encoding as
/// `to_mdast_wire_nopos_opts` (tag byte, child counts, heading depth, list
/// flags+start, listItem/defListDescription spread bool, reftype byte, directive
/// attrs — no position) EXCEPT each string is replaced by just `[u32 u16Len]`
/// (its UTF-16 code-unit length) and each `Option<String>` by `[u32 u16Len]` for
/// `Some` or `[u32 0xFFFFFFFF]` for `None`. The string bytes live only in the pool.
///
/// The `pool` is every string's UTF-8 bytes concatenated in the exact
/// preorder-DFS order the strings are emitted into the structure. A reader walking
/// the structure in order, tracking a running UTF-16 offset, recovers each string
/// as `pool[runningOff .. runningOff + u16Len]` (offsets and lengths in UTF-16
/// units, which is what JS `String.prototype.slice` uses).
/// A reusable context for the string-pooled wire fast path
/// ([`to_mdast_wire_fast_opts`]) that keeps the recyclable working buffers — the
/// block node arena, text buffer, reference map, inline `Scratch`, and string
/// pool — warm across calls, mirroring [`crate::Renderer`] for the HTML path.
///
/// Use it instead of [`to_mdast_wire_fast_opts`] for repeated emits (a long-lived
/// wasm instance handling many documents) to avoid re-allocating those buffers on
/// every call. The returned wire `Vec<u8>` is always freshly allocated (it is
/// leaked across the wasm→JS boundary), but every internal scratch is reused.
#[cfg(feature = "ast")]
pub struct WireFast {
    nodes: Vec<crate::block::Node>,
    buf: String,
    refmap: crate::inline::RefMap,
    scratch: Scratch,
    pool: Vec<u8>,
}

#[cfg(feature = "ast")]
impl Default for WireFast {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "ast")]
impl WireFast {
    /// Create an empty context; its buffers grow to fit the first emit and are
    /// reused thereafter.
    pub fn new() -> Self {
        WireFast {
            nodes: Vec::new(),
            buf: String::new(),
            refmap: crate::inline::RefMap::new(),
            scratch: Scratch::new(),
            pool: Vec::new(),
        }
    }

    /// Parse `src` and serialize the string-pooled wire, reusing the held buffers.
    /// Byte-identical to [`to_mdast_wire_fast_opts`]`(src, opts)`.
    pub fn emit(&mut self, src: &str, opts: crate::Options) -> Vec<u8> {
        let nodes = core::mem::take(&mut self.nodes);
        let buf = core::mem::take(&mut self.buf);
        let refmap = core::mem::take(&mut self.refmap);
        let tree = crate::block::parse_with(src, opts, nodes, buf, refmap);

        // Reuse the held Scratch in place of a fresh `fn_scratch(&tree)`: clear any
        // document-level state that a fresh `Scratch::new()` would start empty, and
        // re-seed the footnote id set exactly as `fn_scratch` does. The transient
        // inline state (list/stack/cur/sem) is reset inside `render_inline` per
        // inline run; `resolve`/`toks` are toggled there too. Clearing the rest here
        // makes the reused Scratch byte-identical to a fresh one.
        self.scratch.slugs.clear();
        #[cfg(feature = "footnotes")]
        {
            self.scratch.footnote_order.clear();
            self.scratch.footnote_seen.clear();
            if tree.opts.footnotes {
                self.scratch.footnote_ids.clone_from(&tree.footnote_ids);
            } else {
                self.scratch.footnote_ids.clear();
            }
        }

        let mut pool = core::mem::take(&mut self.pool);
        pool.clear();
        pool.reserve(src.len());
        let ctx = PosCtx::pooled_with(pool);

        // Same layout as `to_mdast_wire_fast_opts`: a 4-byte poolStart placeholder,
        // then the structure, then the pool appended, then backpatch poolStart.
        let mut out = Vec::<u8>::with_capacity(src.len() * 2 + 64);
        out.extend_from_slice(&[0u8; 4]);
        bwire(&tree, tree.root, &mut self.scratch, &ctx, &mut out);
        let pool = ctx
            .pool
            .expect("pooled ctx always carries a pool")
            .into_inner();
        let pool_start = out.len() as u32;
        out.extend_from_slice(&pool);
        w_u32_at(&mut out, 0, pool_start);

        // Recycle: store the pool back, reclaim the block buffers from the tree, and
        // keep the Scratch (it was reused in place).
        self.pool = pool;
        (self.nodes, self.buf, self.refmap) = tree.recycle();
        out
    }
}

pub fn to_mdast_wire_fast_opts(src: &str, opts: crate::Options) -> Vec<u8> {
    let tree = crate::block::parse_with_opts(src, opts);
    let mut scratch = fn_scratch(&tree);
    let ctx = PosCtx::pooled();
    // The pool holds only inline text, so it never exceeds the source length;
    // reserve once up front to avoid ~log2(N) growth reallocs as strings append.
    ctx.pool
        .as_ref()
        .expect("pooled ctx always carries a pool")
        .borrow_mut()
        .reserve(src.len());
    // Build the structure directly into `out`, after a 4-byte poolStart placeholder,
    // so it is never copied a second time; backpatch poolStart once the pool offset
    // (the structure's end) is known.
    let mut out = Vec::<u8>::with_capacity(src.len() * 2 + 64);
    out.extend_from_slice(&[0u8; 4]);
    bwire(&tree, tree.root, &mut scratch, &ctx, &mut out);
    let pool = ctx
        .pool
        .expect("pooled ctx always carries a pool")
        .into_inner();
    let pool_start = out.len() as u32;
    out.extend_from_slice(&pool);
    w_u32_at(&mut out, 0, pool_start);
    out
}

#[inline]
fn w_u32(v: u32, out: &mut Vec<u8>) {
    out.extend_from_slice(&v.to_le_bytes());
}
/// A string's UTF-16 code-unit length: `s.len()` is exact for ASCII (each byte is
/// one UTF-16 unit), else count the encoded units.
#[inline]
fn u16_len(s: &str) -> u32 {
    if s.is_ascii() {
        s.len() as u32
    } else {
        s.encode_utf16().count() as u32
    }
}
/// Write a string to wire. In the inline format this is `[u32 hdr][utf8 bytes]`
/// where `hdr` carries the byte length plus an ASCII high-bit flag. In the pooled
/// format (`ctx.pool` is `Some`) the structure gets only `[u32 u16Len]` and the
/// UTF-8 bytes are appended to the shared pool in DFS emit order.
fn w_str(s: &str, ctx: &PosCtx, out: &mut Vec<u8>) {
    if let Some(pool) = &ctx.pool {
        w_u32(u16_len(s), out);
        pool.borrow_mut().extend_from_slice(s.as_bytes());
        return;
    }
    // Encode the ASCII-ness in the length's high bit so the JS reader can pick
    // the fast `String.fromCharCode` path without re-scanning the bytes itself
    // (Rust's `is_ascii` is a cheap vectorized scan). Lengths are « 2^31.
    let hdr = (s.len() as u32) | if s.is_ascii() { 0x8000_0000 } else { 0 };
    w_u32(hdr, out);
    out.extend_from_slice(s.as_bytes());
}
fn w_opt(o: &Option<String>, ctx: &PosCtx, out: &mut Vec<u8>) {
    match o {
        Some(s) => w_str(s, ctx, out),
        None => w_u32(u32::MAX, out),
    }
}
fn w_opt_str(o: Option<&str>, ctx: &PosCtx, out: &mut Vec<u8>) {
    match o {
        Some(s) => w_str(s, ctx, out),
        None => w_u32(u32::MAX, out),
    }
}
fn reftype_code(r: &str) -> u8 {
    match r {
        "shortcut" => 0,
        "collapsed" => 1,
        _ => 2, // "full"
    }
}
#[inline]
fn w_u32_at(out: &mut [u8], off: usize, v: u32) {
    out[off..off + 4].copy_from_slice(&v.to_le_bytes());
}
#[inline]
fn reserve(out: &mut Vec<u8>, n: usize) -> usize {
    let off = out.len();
    out.resize(off + n, 0);
    off
}
fn w_point(out: &mut Vec<u8>, pt: (u32, u32, u32)) {
    w_u32(pt.0, out);
    w_u32(pt.1, out);
    w_u32(pt.2, out);
}
fn patch_point(out: &mut [u8], off: usize, pt: (u32, u32, u32)) {
    w_u32_at(out, off, pt.0);
    w_u32_at(out, off + 4, pt.1);
    w_u32_at(out, off + 8, pt.2);
}

/// Sentinel returned by [`reserve_pos`] / accepted by [`patch_pos`] when position
/// is disabled — no end-point slot was reserved, so the later patch is a no-op.
const POS_DISABLED: usize = usize::MAX;

/// Emit a start/end position point for source byte `boff`, but only when position
/// is enabled. When disabled this writes nothing AND never calls `ctx.point`
/// (which would index `PosCtx`'s empty tables on the disabled path).
#[inline]
fn w_pos(ctx: &PosCtx, out: &mut Vec<u8>, boff: usize) {
    if ctx.enabled {
        w_point(out, ctx.point(boff));
    }
}

/// Reserve a 12-byte end-position slot for later backpatching — but only when
/// position is enabled. When disabled it reserves nothing and returns
/// [`POS_DISABLED`], so the matching [`patch_pos`] becomes a no-op.
#[inline]
fn reserve_pos(ctx: &PosCtx, out: &mut Vec<u8>) -> usize {
    if ctx.enabled {
        reserve(out, 12)
    } else {
        POS_DISABLED
    }
}

/// Backpatch a reserved end-position slot with the point for source byte `boff`.
/// A no-op when `off == POS_DISABLED` (position disabled), and `ctx.point` is
/// never called on that path.
#[inline]
fn patch_pos(ctx: &PosCtx, out: &mut [u8], off: usize, boff: usize) {
    if ctx.enabled && off != POS_DISABLED {
        patch_point(out, off, ctx.point(boff));
    }
}

/// Emit one block node's wire bytes; return its source byte end (parents whose
/// end depends on the last child use it). Mirrors [`block`] byte-for-byte; ends
/// and child counts that depend on children are backpatched.
fn bwire(tree: &Tree, idx: usize, scratch: &mut Scratch, ctx: &PosCtx, out: &mut Vec<u8>) -> usize {
    let node = &tree.nodes[idx];
    let (s, e) = tree.src_span(idx);
    let (sb, se) = (s as usize, e as usize);
    match node.kind {
        Kind::Document => {
            let eb = ctx.src_len;
            out.push(0);
            w_pos(ctx, out, 0);
            w_pos(ctx, out, eb);
            let coff = reserve(out, 4);
            let n = bchildren(tree, idx, scratch, ctx, out).0;
            w_u32_at(out, coff, n);
            eb
        }
        Kind::Paragraph => {
            let eb = ctx.rtrim_nl(se);
            out.push(1);
            w_pos(ctx, out, sb);
            w_pos(ctx, out, eb);
            let coff = reserve(out, 4);
            let n = inline_wire(tree, idx, scratch, ctx, sb, eb, out);
            w_u32_at(out, coff, n);
            eb
        }
        Kind::Heading => {
            let eb = ctx.rtrim_nl(se);
            out.push(2);
            w_pos(ctx, out, sb);
            w_pos(ctx, out, eb);
            out.push(node.level);
            let coff = reserve(out, 4);
            let n = inline_wire(tree, idx, scratch, ctx, sb, eb, out);
            w_u32_at(out, coff, n);
            eb
        }
        Kind::BlockQuote => {
            out.push(3);
            w_pos(ctx, out, sb);
            let eoff = reserve_pos(ctx, out);
            let coff = reserve(out, 4);
            let (n, last) = bchildren(tree, idx, scratch, ctx, out);
            let end = last.map_or(se, |l| l.max(se));
            patch_pos(ctx, out, eoff, end);
            w_u32_at(out, coff, n);
            end
        }
        Kind::List => {
            let ld = node.list.as_ref().unwrap();
            out.push(4);
            w_pos(ctx, out, sb);
            let eoff = reserve_pos(ctx, out);
            out.push((ld.ordered as u8) | ((ld.spread as u8) << 1));
            w_u32(
                if ld.ordered {
                    ld.start as u32
                } else {
                    u32::MAX
                },
                out,
            );
            let coff = reserve(out, 4);
            let (n, last) = bchildren(tree, idx, scratch, ctx, out);
            // Matches block(): a list can extend past its last item (it absorbs a
            // trailing blockquote-marker blank line, tracked in `se`).
            let end = last.map_or(se, |l| l.max(se));
            patch_pos(ctx, out, eoff, end);
            w_u32_at(out, coff, n);
            end
        }
        Kind::Item => {
            out.push(5);
            w_pos(ctx, out, sb);
            let eoff = reserve_pos(ctx, out);
            out.push(node.item_spread as u8);
            // GFM task list: a `[ ]`/`[x]` marker (+ one whitespace) at the very
            // start of the item's first paragraph sets `checked` and is stripped
            // from that paragraph. `2` encodes mdast's `checked: null` (not a task).
            let task = task_marker(tree, idx);
            out.push(match task {
                Some((true, ..)) => 1,
                Some((false, ..)) => 0,
                None => 2,
            });
            let coff = reserve(out, 4);
            let (n, last) = match task {
                Some((_, strip, para)) => task_item_children(tree, para, strip, scratch, ctx, out),
                None => bchildren(tree, idx, scratch, ctx, out),
            };
            let end = last.unwrap_or(se);
            patch_pos(ctx, out, eoff, end);
            w_u32_at(out, coff, n);
            end
        }
        Kind::ThematicBreak => {
            let eb = ctx.rtrim_nl(se);
            out.push(6);
            w_pos(ctx, out, sb);
            w_pos(ctx, out, eb);
            eb
        }
        Kind::CodeBlock => {
            let (lang, meta) = code_info(tree.info(idx));
            let mut value = tree.content(idx).to_owned();
            if value.ends_with('\n') {
                value.pop();
            }
            let eb = if node.fenced {
                se
            } else {
                ctx.rtrim_code_end(sb, se)
            };
            out.push(7);
            w_pos(ctx, out, sb);
            w_pos(ctx, out, eb);
            w_opt(&lang, ctx, out);
            w_opt(&meta, ctx, out);
            w_str(&value, ctx, out);
            eb
        }
        Kind::HtmlBlock => {
            let eb = tree.html_ast_end(idx) as usize;
            out.push(8);
            w_pos(ctx, out, sb);
            w_pos(ctx, out, eb);
            w_str(tree.html_value(idx), ctx, out);
            eb
        }
        // The `Frontmatter` variant is always compiled (to keep the hot block
        // matches stable), but a node only exists with the `frontmatter` feature.
        #[cfg(not(feature = "frontmatter"))]
        Kind::Frontmatter => unreachable!("Frontmatter node requires the `frontmatter` feature"),
        #[cfg(feature = "frontmatter")]
        Kind::Frontmatter => {
            let value = frontmatter_value(tree.content(idx));
            out.push(if node.level == 1 { 21 } else { 20 }); // 20 = yaml, 21 = toml
            w_pos(ctx, out, sb);
            w_pos(ctx, out, se);
            w_str(value, ctx, out);
            se
        }
        // The `FootnoteDef` variant is always compiled (to keep the hot block
        // matches stable), but a node only exists with the `footnotes` feature.
        #[cfg(not(feature = "footnotes"))]
        Kind::FootnoteDef => unreachable!("FootnoteDef node requires the `footnotes` feature"),
        #[cfg(feature = "footnotes")]
        Kind::FootnoteDef => {
            let d = tree.fn_def(idx);
            out.push(22);
            w_pos(ctx, out, sb);
            let eoff = reserve_pos(ctx, out);
            w_str(&d.identifier, ctx, out);
            w_str(&d.label, ctx, out);
            let coff = reserve(out, 4);
            let (n, last) = bchildren(tree, idx, scratch, ctx, out);
            let end = last.unwrap_or(se);
            patch_pos(ctx, out, eoff, end);
            w_u32_at(out, coff, n);
            end
        }
        Kind::Definition => {
            let d = tree.definition(idx);
            let eb = ctx.line_content_end(ctx.rtrim(sb, se));
            out.push(17);
            w_pos(ctx, out, sb);
            w_pos(ctx, out, eb);
            w_str(&d.identifier, ctx, out);
            w_str(&d.label, ctx, out);
            w_str(&d.url, ctx, out);
            w_opt(&d.title, ctx, out);
            eb
        }
        // The deflist variants stay compiled (to keep the hot block match
        // stable), but a node only exists with the `deflist` feature.
        #[cfg(not(feature = "deflist"))]
        Kind::DefList | Kind::DefTerm | Kind::DefDesc => {
            unreachable!("DefList nodes require the `deflist` feature")
        }
        #[cfg(feature = "deflist")]
        Kind::DefList => {
            // tag 24 = defList; children are built from the term/description
            // nodes the same way as `block()` (terms split per line).
            out.push(24);
            w_pos(ctx, out, sb);
            let eoff = reserve_pos(ctx, out);
            let coff = reserve(out, 4);
            let mut n = 0u32;
            let mut last = se;
            let mut c = tree.first_child(idx);
            while let Some(ci) = c {
                match tree.nodes[ci].kind {
                    Kind::DefTerm => {
                        let content = tree.content(ci);
                        let mut co = 0usize;
                        for line in content.split_inclusive('\n') {
                            let lead = line.len() - line.trim_start_matches([' ', '\t']).len();
                            let body = line.trim_matches([' ', '\t', '\n', '\r']);
                            if !body.is_empty() {
                                let off = (co + lead) as u32;
                                let sbb = tree.content_to_src(ci, off) as usize;
                                out.push(25); // defListTerm
                                w_pos(ctx, out, sbb);
                                let teoff = reserve_pos(ctx, out);
                                let tcoff = reserve(out, 4);
                                let (ebb, tn) =
                                    inline_wire_slice(tree, ci, off, body, scratch, ctx, out);
                                patch_pos(ctx, out, teoff, ebb);
                                w_u32_at(out, tcoff, tn);
                                last = ebb;
                                n += 1;
                            }
                            co += line.len();
                        }
                    }
                    Kind::DefDesc => {
                        let spread = tree.nodes[ci].level == 1;
                        let content = tree.content(ci);
                        let lead =
                            content.len() - content.trim_start_matches([' ', '\t', '\n']).len();
                        let body = content.trim_matches([' ', '\t', '\n', '\r']);
                        let off = lead as u32;
                        let sbb = tree.content_to_src(ci, off) as usize;
                        out.push(26); // defListDescription
                        w_pos(ctx, out, sbb);
                        let deoff = reserve_pos(ctx, out);
                        out.push(spread as u8);
                        let dcoff = reserve(out, 4);
                        // Single child: a paragraph wrapping the inline body.
                        out.push(1); // paragraph
                        w_pos(ctx, out, sbb);
                        let peoff = reserve_pos(ctx, out);
                        let pcoff = reserve(out, 4);
                        let (ebb, pn) = inline_wire_slice(tree, ci, off, body, scratch, ctx, out);
                        patch_pos(ctx, out, peoff, ebb);
                        w_u32_at(out, pcoff, pn);
                        patch_pos(ctx, out, deoff, ebb);
                        w_u32_at(out, dcoff, 1);
                        last = ebb;
                        n += 1;
                    }
                    _ => {
                        last = bwire(tree, ci, scratch, ctx, out);
                        n += 1;
                    }
                }
                c = tree.next_sibling(ci);
            }
            patch_pos(ctx, out, eoff, last);
            w_u32_at(out, coff, n);
            last
        }
        // The directive variants stay compiled (to keep the hot block match
        // stable), but a node only exists with the `directives` feature.
        #[cfg(not(feature = "directives"))]
        Kind::LeafDirective | Kind::ContainerDirective => {
            unreachable!("directive nodes require the `directives` feature")
        }
        #[cfg(feature = "directives")]
        Kind::LeafDirective => {
            let d = tree.directive(idx);
            let eb = ctx.rtrim_nl(se);
            out.push(28); // leafDirective
            w_pos(ctx, out, sb);
            w_pos(ctx, out, eb);
            w_str(&d.name, ctx, out);
            w_attrs(&d.attrs, ctx, out);
            let coff = reserve(out, 4);
            let n = match d.label {
                Some((ls, le)) => {
                    inline_wire_src(tree, ls, tree.source_range(ls, le), scratch, ctx, out).1
                }
                None => 0,
            };
            w_u32_at(out, coff, n);
            eb
        }
        #[cfg(feature = "directives")]
        Kind::ContainerDirective => {
            let d = tree.directive(idx);
            out.push(29); // containerDirective
            w_pos(ctx, out, sb);
            let eoff = reserve_pos(ctx, out);
            w_str(&d.name, ctx, out);
            w_attrs(&d.attrs, ctx, out);
            let coff = reserve(out, 4);
            let mut n = 0u32;
            let mut last = se;
            if let Some((ls, le)) = d.label {
                // directiveLabel paragraph (tag 30).
                out.push(30);
                w_pos(ctx, out, ls as usize);
                let leoff = reserve_pos(ctx, out);
                let lcoff = reserve(out, 4);
                let (lend, ln) =
                    inline_wire_src(tree, ls, tree.source_range(ls, le), scratch, ctx, out);
                patch_pos(ctx, out, leoff, lend);
                w_u32_at(out, lcoff, ln);
                n += 1;
            }
            let (bn, blast) = bchildren(tree, idx, scratch, ctx, out);
            n += bn;
            if let Some(l) = blast {
                last = last.max(l);
            }
            patch_pos(ctx, out, eoff, last);
            w_u32_at(out, coff, n);
            last
        }
        // Built inline by their `DefList` parent; unreachable but kept exhaustive.
        #[cfg(feature = "deflist")]
        Kind::DefTerm => {
            let eb = ctx.rtrim_nl(se);
            out.push(25);
            w_pos(ctx, out, sb);
            w_pos(ctx, out, eb);
            let coff = reserve(out, 4);
            let n = inline_wire(tree, idx, scratch, ctx, sb, eb, out);
            w_u32_at(out, coff, n);
            eb
        }
        #[cfg(feature = "deflist")]
        Kind::DefDesc => {
            let eb = ctx.rtrim_nl(se);
            out.push(26);
            w_pos(ctx, out, sb);
            w_pos(ctx, out, eb);
            out.push((node.level == 1) as u8);
            w_u32(0, out);
            eb
        }
        #[cfg(feature = "gfm")]
        Kind::Table => bwire_table(tree, idx, scratch, ctx, out),
    }
}

fn bchildren(
    tree: &Tree,
    idx: usize,
    scratch: &mut Scratch,
    ctx: &PosCtx,
    out: &mut Vec<u8>,
) -> (u32, Option<usize>) {
    let mut n = 0u32;
    let mut last = None;
    let mut c = tree.first_child(idx);
    while let Some(ci) = c {
        last = Some(bwire(tree, ci, scratch, ctx, out));
        n += 1;
        c = tree.next_sibling(ci);
    }
    (n, last)
}

/// GFM task list (`mdast-util-gfm-task-list-item`): if list `item`'s first child
/// is a paragraph beginning with `[ ]`/`[x]`/`[X]` *followed by whitespace*, return
/// `(checked, strip, paragraph)` where `strip` is the marker + one whitespace byte
/// to drop. A bare `[ ]` with no trailing whitespace is literal text (not a task),
/// matching remark — note this is stricter than the HTML render's `task_input`.
fn task_marker(tree: &Tree, item: usize) -> Option<(bool, usize, usize)> {
    if !tree.opts.tasklist {
        return None;
    }
    let para = tree.first_child(item)?;
    if tree.nodes[para].kind != Kind::Paragraph {
        return None;
    }
    let s = tree.content(para).as_bytes();
    if s.len() < 4 || s[0] != b'[' || s[2] != b']' || !matches!(s[3], b' ' | b'\t' | b'\n') {
        return None;
    }
    let checked = match s[1] {
        b' ' => false,
        b'x' | b'X' => true,
        _ => return None,
    };
    // The marker must be followed by real (non-whitespace) content; a bare `[ ]`
    // with only trailing whitespace is literal text, not a task (matches micromark).
    if s[4..].iter().all(u8::is_ascii_whitespace) {
        return None;
    }
    Some((checked, 4, para))
}

/// Emit a task-list item's children: the first paragraph with its `[x] ` marker
/// stripped, then the remaining children verbatim. Returns the child count and the
/// item's source end.
fn task_item_children(
    tree: &Tree,
    para: usize,
    strip: usize,
    scratch: &mut Scratch,
    ctx: &PosCtx,
    out: &mut Vec<u8>,
) -> (u32, Option<usize>) {
    out.push(1); // paragraph
    let p_start_off = reserve_pos(ctx, out);
    let (_ps, pe) = tree.src_span(para);
    let p_end = ctx.rtrim_nl(pe as usize);
    w_pos(ctx, out, p_end);
    let pcoff = reserve(out, 4);
    let children_at = out.len();
    let body = &tree.content(para)[strip..];
    let (_e, pn) = inline_wire_slice(tree, para, strip as u32, body, scratch, ctx, out);
    w_u32_at(out, pcoff, pn);
    // `mdast-util-gfm-task-list-item` moves the paragraph start past the stripped
    // `[x] ` only when the first inline child survives as a text node; if that child
    // is a construct (its whitespace-only text node was dropped) the paragraph keeps
    // its original `[` start. Tag 9 == text in the wire.
    let first_is_text = pn > 0 && out[children_at] == 9;
    let p_start = tree.content_to_src(para, if first_is_text { strip as u32 } else { 0 });
    patch_pos(ctx, out, p_start_off, p_start as usize);

    let mut n = 1u32;
    let mut last = Some(p_end);
    let mut c = tree.next_sibling(para);
    while let Some(ci) = c {
        last = Some(bwire(tree, ci, scratch, ctx, out));
        n += 1;
        c = tree.next_sibling(ci);
    }
    (n, last)
}

/// Emit a text block's inline children straight to wire; return the child count.
/// Drives a [`WireSink`] over the resolved inline list via [`render_inline_to_sink`]
/// — no intermediate [`SpanTok`] vector, no per-span `String`: the sink copies
/// borrowed slices straight into `out`, backpatching container ends/counts at close.
fn inline_wire(
    tree: &Tree,
    idx: usize,
    scratch: &mut Scratch,
    ctx: &PosCtx,
    sb: usize,
    eb: usize,
    out: &mut Vec<u8>,
) -> u32 {
    let mut sink = WireSink {
        out,
        tree,
        idx,
        obase: 0,
        src_base: None,
        ctx,
        bpos: ctx.bpos(sb, eb),
        stack: Vec::new(),
        top: 0,
        al: crate::Options::GFM && tree.opts.autolink,
        link_depth: 0,
    };
    render_inline_to_sink(
        tree.content(idx),
        &tree.refmap,
        scratch,
        tree.opts,
        &mut sink,
    );
    sink.top
}

/// Emit the inline content of a raw source range `body` (starting at source byte
/// `src_start`) to wire — used for a block directive's `[label]`. Returns the
/// label's source byte end and its child count.
#[cfg(feature = "directives")]
fn inline_wire_src(
    tree: &Tree,
    src_start: u32,
    body: &str,
    scratch: &mut Scratch,
    ctx: &PosCtx,
    out: &mut Vec<u8>,
) -> (usize, u32) {
    let end = src_start as usize + body.len();
    let mut sink = WireSink {
        out,
        tree,
        idx: 0,
        obase: 0,
        src_base: Some(src_start),
        ctx,
        bpos: ctx.bpos(src_start as usize, end),
        stack: Vec::new(),
        top: 0,
        al: crate::Options::GFM && tree.opts.autolink,
        link_depth: 0,
    };
    render_inline_to_sink(body, &tree.refmap, scratch, tree.opts, &mut sink);
    (end, sink.top)
}

/// Write an ordered attribute object to wire: `u32` count, then `(key, value)`
/// string pairs.
#[cfg(feature = "directives")]
fn w_attrs(attrs: &[(String, String)], ctx: &PosCtx, out: &mut Vec<u8>) {
    w_u32(attrs.len() as u32, out);
    for (k, v) in attrs {
        w_str(k, ctx, out);
        w_str(v, ctx, out);
    }
}

/// Like [`inline_wire`] but for a single inline run `body` that starts at content
/// offset `content_off` within node `ci` — the term/description lines of a
/// definition list. Returns the run's source byte end and the child count.
fn inline_wire_slice(
    tree: &Tree,
    ci: usize,
    content_off: u32,
    body: &str,
    scratch: &mut Scratch,
    ctx: &PosCtx,
    out: &mut Vec<u8>,
) -> (usize, u32) {
    let sbb = tree.content_to_src(ci, content_off) as usize;
    let ebb = tree.content_to_src(ci, content_off + body.len() as u32) as usize;
    let mut sink = WireSink {
        out,
        tree,
        idx: ci,
        obase: content_off,
        src_base: None,
        ctx,
        bpos: ctx.bpos(sbb, ebb),
        stack: Vec::new(),
        top: 0,
        al: crate::Options::GFM && tree.opts.autolink,
        link_depth: 0,
    };
    render_inline_to_sink(body, &tree.refmap, scratch, tree.opts, &mut sink);
    (ebb, sink.top)
}

/// GFM pipe table → wire: `table` (tag 31, an `align` array + `tableRow` kids),
/// each `tableRow` (tag 32), each `tableCell` (tag 33, inline kids). Matches
/// `mdast-util-gfm-table`: the delimiter row sets alignment and is not emitted;
/// every data row keeps the cells it actually has — short rows are not padded and
/// long rows are not truncated (only the HTML render normalizes to the column count).
#[cfg(feature = "gfm")]
fn bwire_table(
    tree: &Tree,
    idx: usize,
    scratch: &mut Scratch,
    ctx: &PosCtx,
    out: &mut Vec<u8>,
) -> usize {
    let content = tree.content(idx);
    // Non-blank lines with their content-byte offsets. A Table node only exists
    // once a header + delimiter row validated, so there are always >= 2 lines.
    let mut lines: Vec<(usize, &str)> = Vec::new();
    let mut o = 0usize;
    for line in content.split('\n') {
        if !line.trim().is_empty() {
            lines.push((o, line));
        }
        o += line.len() + 1;
    }
    let header = lines[0].1;
    let hcells = scan_cells(header);
    let ncols = hcells.len();
    // Column alignments from the delimiter row's trimmed cells.
    let delim = lines[1].1;
    let mut aligns: Vec<u8> = scan_cells(delim)
        .iter()
        .map(|&(_, _, cs, ce)| {
            let t = &delim.as_bytes()[cs as usize..ce as usize];
            match (t.first() == Some(&b':'), t.last() == Some(&b':')) {
                (true, true) => 3,  // center
                (true, false) => 1, // left
                (false, true) => 2, // right
                (false, false) => 0,
            }
        })
        .collect();
    aligns.resize(ncols, 0);

    out.push(31); // table
    let t_start = tree.content_to_src(idx, lines[0].0 as u32 + hcells[0].0) as usize;
    w_pos(ctx, out, t_start);
    let teoff = reserve_pos(ctx, out);
    w_u32(ncols as u32, out);
    out.extend_from_slice(&aligns);
    let tcoff = reserve(out, 4);

    // Rows: the header line, then every data line (lines[2..]). The delimiter
    // (lines[1]) is consumed for alignment and never emitted as a row.
    let mut nrows = 1u32;
    bwire_table_row(tree, idx, lines[0].0, header, scratch, ctx, out);
    for &(lo, body) in &lines[2..] {
        bwire_table_row(tree, idx, lo, body, scratch, ctx, out);
        nrows += 1;
    }
    w_u32_at(out, tcoff, nrows);

    // Table spans to the end of its last source line's last cell (the delimiter
    // row when there are no data rows) — same cell-end convention as the rows.
    let (llo, lbody) = *lines.last().unwrap();
    let lend = scan_cells(lbody).last().map_or(0, |c| c.1);
    let t_end = tree.content_to_src(idx, llo as u32 + lend) as usize;
    patch_pos(ctx, out, teoff, t_end);
    t_end
}

/// Emit one `tableRow` (tag 32) and its `tableCell` children. `lo` is the row
/// line's offset within the table node content. Like `mdast-util-gfm-table`, every
/// cell the row actually has is kept — short rows are not padded and long rows are
/// not truncated (only the HTML render pads/truncates to the column count).
#[cfg(feature = "gfm")]
fn bwire_table_row(
    tree: &Tree,
    idx: usize,
    lo: usize,
    body: &str,
    scratch: &mut Scratch,
    ctx: &PosCtx,
    out: &mut Vec<u8>,
) {
    let cells = scan_cells(body);
    out.push(32); // tableRow
    // The row runs from the first cell's start (after any leading line whitespace)
    // to the last cell's end — which `scan_cells` carries to the raw line end, so a
    // trailing pipe and any trailing whitespace ARE inside the row span (matching
    // `mdast-util-gfm-table`, which trims only the leading edge, not the trailing tail).
    let r_start = tree.content_to_src(idx, lo as u32 + cells[0].0) as usize;
    w_pos(ctx, out, r_start);
    let reoff = reserve_pos(ctx, out);
    let rcoff = reserve(out, 4);
    for &(ns, ne, cs, ce) in &cells {
        out.push(33); // tableCell
        let c_start = tree.content_to_src(idx, lo as u32 + ns) as usize;
        w_pos(ctx, out, c_start);
        let ceoff = reserve_pos(ctx, out);
        let ccoff = reserve(out, 4);
        let cell = &body[cs as usize..ce as usize];
        let (_end, nk) = inline_wire_slice(tree, idx, lo as u32 + cs, cell, scratch, ctx, out);
        w_u32_at(out, ccoff, nk);
        let c_end = tree.content_to_src(idx, lo as u32 + ne) as usize;
        patch_pos(ctx, out, ceoff, c_end);
    }
    w_u32_at(out, rcoff, cells.len() as u32);
    let r_end = tree.content_to_src(idx, lo as u32 + cells.last().unwrap().1) as usize;
    patch_pos(ctx, out, reoff, r_end);
}

/// GFM pipe-table cell spans for one row line, all **line-relative** byte offsets
/// `(node_start, node_end, content_start, content_end)`. Node spans tile the line
/// edge-to-edge at the interior (separator) pipes — a leading pipe folds into the
/// first cell, a trailing pipe into the last — matching `mdast-util-gfm-table`.
/// Content spans are the cell text trimmed of spaces/tabs (inline-parsed); `\|`
/// is not a separator (it is later handled as a backslash escape).
#[cfg(feature = "gfm")]
fn scan_cells(body: &str) -> Vec<(u32, u32, u32, u32)> {
    let b = body.as_bytes();
    // The row spans only its non-whitespace extent `[e0, e1)` — leading/trailing
    // line whitespace is outside every cell (matching `mdast-util-gfm-table`, whose
    // row/first-cell positions start at the first non-space, not the line start).
    let Some(e0) = b.iter().position(|&c| c != b' ' && c != b'\t') else {
        return Vec::new(); // all-whitespace line (filtered out before emit)
    };
    let e1 = b.iter().rposition(|&c| c != b' ' && c != b'\t').unwrap() + 1;

    let mut pipes: Vec<usize> = Vec::new();
    let mut esc = false;
    for (i, &c) in b.iter().enumerate() {
        if esc {
            esc = false;
        } else if c == b'\\' {
            esc = true;
        } else if c == b'|' {
            pipes.push(i);
        }
    }
    let has_leading = pipes.first() == Some(&e0);
    let has_trailing = pipes.last() == Some(&(e1 - 1));
    let lo = usize::from(has_leading);
    let hi = pipes.len() - usize::from(has_trailing);
    let interior: &[usize] = if lo <= hi { &pipes[lo..hi] } else { &[] };

    let count = interior.len() + 1;
    let mut cells = Vec::with_capacity(count);
    for k in 0..count {
        let ns = if k == 0 { e0 } else { interior[k - 1] };
        // The first cell starts at the first non-whitespace (`e0`), but the last
        // cell always runs to the raw line end `n` — including a closing pipe and
        // any trailing whitespace. (`mdast-util-gfm-table` trims the leading edge
        // but not the trailing tail.) Interior cells end at the next pipe.
        let ne = if k == count - 1 { b.len() } else { interior[k] };
        let craw_l = if k == 0 {
            if has_leading { pipes[0] + 1 } else { e0 }
        } else {
            interior[k - 1] + 1
        };
        let craw_r = (if k == count - 1 {
            if has_trailing {
                *pipes.last().unwrap()
            } else {
                e1
            }
        } else {
            interior[k]
        })
        .max(craw_l);
        let mut cs = craw_l;
        let mut ce = craw_r;
        while cs < ce && (b[cs] == b' ' || b[cs] == b'\t') {
            cs += 1;
        }
        while ce > cs && (b[ce - 1] == b' ' || b[ce - 1] == b'\t') {
            ce -= 1;
        }
        cells.push((ns as u32, ne as u32, cs as u32, ce as u32));
    }
    cells
}

/// An open inline container: byte offsets to backpatch (end position + child
/// count) plus the opener's content span and running child count.
struct InlineFrame {
    eoff: usize,
    coff: usize,
    os: u32,
    oe: u32,
    count: u32,
    /// True for a `link`/`linkReference` frame, so `close` can keep `link_depth`
    /// (the "ignore inside link" guard for the autolink transform) accurate.
    is_link: bool,
}

/// [`InlineSink`] that writes the binary wire form directly. Container nesting
/// from the sink's `open`/`close` pairs is reconstructed with `stack`; each open
/// reserves its end-position + child-count slots, patched when it closes.
struct WireSink<'a> {
    out: &'a mut Vec<u8>,
    tree: &'a Tree<'a>,
    idx: usize,
    /// Offset added to every content offset before mapping to source — non-zero
    /// when emitting a sub-slice (a definition-list term/description line) whose
    /// token offsets are relative to the slice, not the node's content.
    obase: u32,
    /// When `Some(base)`, offsets map directly to source as `base + off` (used for
    /// a directive `[label]`, a raw source range with no owning content node).
    src_base: Option<u32>,
    ctx: &'a PosCtx,
    bpos: Pos,
    stack: Vec<InlineFrame>,
    top: u32,
    /// GFM autolink-literal transform enabled (`Options::GFM && opts.autolink`).
    al: bool,
    /// Nesting depth of `link`/`linkReference` ancestors; the transform skips text
    /// while `> 0` (`ignore: ['link','linkReference']`).
    link_depth: u32,
}

impl WireSink<'_> {
    #[inline]
    fn map(&self, off: u32) -> usize {
        match self.src_base {
            Some(b) => (b + off) as usize,
            None => self.tree.content_to_src(self.idx, self.obase + off) as usize,
        }
    }
    /// Position for a content span `[s, e)`; `s == u32::MAX` falls back to the
    /// block span (mirrors `inline_wire`'s former `mkpos` closure exactly).
    fn mkpos(&self, s: u32, e: u32) -> Pos {
        if s == u32::MAX {
            self.bpos.clone()
        } else {
            let send = if e == 0 {
                self.map(0)
            } else {
                self.map(e - 1) + 1
            };
            self.ctx.pos(self.map(s), send)
        }
    }
    fn leaf_start(&self, s: u32) -> (u32, u32, u32) {
        if s == u32::MAX {
            self.bpos.start
        } else {
            self.ctx.point(self.map(s))
        }
    }
    #[inline]
    fn bump(&mut self) {
        match self.stack.last_mut() {
            Some(f) => f.count += 1,
            None => self.top += 1,
        }
    }
    /// Common leaf prologue: tag byte + full position (position only when
    /// enabled; on the disabled path `mkpos`/`ctx.point` are never reached).
    fn leaf(&mut self, tag: u8, start: u32, end: u32) {
        self.out.push(tag);
        if self.ctx.enabled {
            let p = self.mkpos(start, end);
            w_point(self.out, p.start);
            w_point(self.out, p.end);
        }
    }
    /// Emit a start point for content span start `start`, only when enabled.
    #[inline]
    fn w_start(&mut self, start: u32) {
        if self.ctx.enabled {
            let ls = self.leaf_start(start);
            w_point(self.out, ls);
        }
    }
    /// Reserve a container's end-position slot, only when enabled (else sentinel).
    #[inline]
    fn reserve_end(&mut self) -> usize {
        if self.ctx.enabled {
            reserve(self.out, 12)
        } else {
            POS_DISABLED
        }
    }
    /// A leaf prologue with NO unist position — for the nodes the GFM autolink
    /// transform creates (`mdast-util-find-and-replace` leaves them position-less).
    /// When positions are on, a sentinel point (`u32::MAX×3`) is written that the
    /// JS reader maps to `position: undefined`.
    fn leaf_nopos(&mut self, tag: u8) {
        self.out.push(tag);
        if self.ctx.enabled {
            let s = (u32::MAX, u32::MAX, u32::MAX);
            w_point(self.out, s);
            w_point(self.out, s);
        }
    }
    /// A position-less `text` node (a transform text piece).
    fn al_text(&mut self, value: &str) {
        self.leaf_nopos(9);
        w_str(value, self.ctx, self.out);
        self.bump();
    }
    /// A position-less autolink `link` (a transform autolink) — one text child,
    /// also position-less. Mirrors the [`autolink`](WireSink::autolink) layout.
    fn al_link(&mut self, url: &str, text: &str) {
        self.leaf_nopos(15);
        w_str(url, self.ctx, self.out);
        w_u32(u32::MAX, self.out); // title: None
        w_u32(1, self.out); // one text child
        self.leaf_nopos(9);
        w_str(text, self.ctx, self.out);
        self.bump();
    }
}

impl InlineSink for WireSink<'_> {
    fn text(&mut self, value: &str, start: u32, end: u32) {
        // GFM autolink-literal transform: outside any link, re-scan the (already
        // coalesced) text node for bare autolinks the tokenizer missed. On a hit,
        // replace it with position-less text/link pieces (matching remark-gfm).
        if self.al
            && self.link_depth == 0
            && let Some(pieces) = crate::inline::al_transform_text(value)
        {
            for piece in pieces {
                match piece {
                    crate::inline::AlPiece::Text(t) => self.al_text(&t),
                    crate::inline::AlPiece::Link { url, text } => self.al_link(&url, &text),
                }
            }
            return;
        }
        self.leaf(9, start, end);
        w_str(value, self.ctx, self.out);
        self.bump();
    }
    fn code(&mut self, value: &str, start: u32, end: u32) {
        self.leaf(13, start, end);
        w_str(value, self.ctx, self.out);
        self.bump();
    }
    fn html(&mut self, value: &str, start: u32, end: u32) {
        self.leaf(8, start, end);
        w_str(value, self.ctx, self.out);
        self.bump();
    }
    fn brk(&mut self, start: u32, end: u32) {
        self.leaf(14, start, end);
        self.bump();
    }
    fn image(&mut self, url: &str, title: Option<&str>, alt: &str, start: u32, end: u32) {
        self.leaf(16, start, end);
        w_str(url, self.ctx, self.out);
        w_opt_str(title, self.ctx, self.out);
        w_str(alt, self.ctx, self.out);
        self.bump();
    }
    fn imageref(
        &mut self,
        identifier: &str,
        label: &str,
        reftype: &'static str,
        alt: &str,
        start: u32,
        end: u32,
    ) {
        self.leaf(19, start, end);
        w_str(identifier, self.ctx, self.out);
        w_str(label, self.ctx, self.out);
        self.out.push(reftype_code(reftype));
        w_str(alt, self.ctx, self.out);
        self.bump();
    }
    #[cfg(feature = "footnotes")]
    fn footnote_ref(&mut self, identifier: &str, label: &str, start: u32, end: u32) {
        self.leaf(23, start, end);
        w_str(identifier, self.ctx, self.out);
        w_str(label, self.ctx, self.out);
        self.bump();
    }
    #[cfg(feature = "directives")]
    fn text_directive(
        &mut self,
        name: &str,
        attrs: &[(String, String)],
        _label: Option<(u32, u32)>,
        start: u32,
        end: u32,
    ) {
        // tag 27 = textDirective. The wire path has no scratch to re-tokenize the
        // `[label]`, so its inline children are emitted empty here (the JSON mdast
        // path — what the directive gate checks — carries the full children).
        self.leaf(27, start, end);
        w_str(name, self.ctx, self.out);
        w_attrs(attrs, self.ctx, self.out);
        w_u32(0, self.out); // children: empty (best-effort on the wire path)
        self.bump();
    }
    fn autolink(&mut self, url: &str, text: &str, start: u32, end: u32) {
        self.leaf(15, start, end);
        w_str(url, self.ctx, self.out);
        w_u32(u32::MAX, self.out); // title: None
        w_u32(1, self.out); // one text child
        self.out.push(9);
        if self.ctx.enabled {
            // The text child spans only the visible text. A `<url>` autolink wraps
            // it in `<>` (one byte each side); a bare GFM autolink has no wrapper.
            // Derive the symmetric padding from the span vs the text length so both
            // land right: `<https://x>` → text [start+1, end-1], `https://x` → text
            // [start, end] (equal to the link span, matching remark-gfm).
            let pad = (((end - start) as usize).saturating_sub(text.len()) / 2) as u32;
            let cp = self.mkpos(start.saturating_add(pad), end.saturating_sub(pad));
            w_point(self.out, cp.start);
            w_point(self.out, cp.end);
        }
        w_str(text, self.ctx, self.out);
        self.bump();
    }
    fn open(&mut self, kind: &'static str, start: u32, end: u32) {
        self.bump();
        let tag = match kind {
            "strong" => 11,
            "delete" => 12,
            _ => 10,
        };
        self.out.push(tag);
        self.w_start(start);
        let eoff = self.reserve_end();
        let coff = reserve(self.out, 4);
        self.stack.push(InlineFrame {
            eoff,
            coff,
            os: start,
            oe: end,
            count: 0,
            is_link: false,
        });
    }
    fn link_open(&mut self, url: &str, title: Option<&str>, start: u32, end: u32) {
        self.bump();
        self.out.push(15);
        self.w_start(start);
        let eoff = self.reserve_end();
        w_str(url, self.ctx, self.out);
        w_opt_str(title, self.ctx, self.out);
        let coff = reserve(self.out, 4);
        self.link_depth += 1;
        self.stack.push(InlineFrame {
            eoff,
            coff,
            os: start,
            oe: end,
            count: 0,
            is_link: true,
        });
    }
    fn linkref_open(
        &mut self,
        identifier: &str,
        label: &str,
        reftype: &'static str,
        start: u32,
        end: u32,
    ) {
        self.bump();
        self.out.push(18);
        self.w_start(start);
        let eoff = self.reserve_end();
        w_str(identifier, self.ctx, self.out);
        w_str(label, self.ctx, self.out);
        self.out.push(reftype_code(reftype));
        let coff = reserve(self.out, 4);
        self.link_depth += 1;
        self.stack.push(InlineFrame {
            eoff,
            coff,
            os: start,
            oe: end,
            count: 0,
            is_link: true,
        });
    }
    fn close(&mut self, _start: u32, end: u32) {
        if let Some(f) = self.stack.pop() {
            if f.is_link {
                self.link_depth -= 1;
            }
            if self.ctx.enabled {
                let cend = if end > f.oe { end } else { f.oe };
                let p = self.mkpos(f.os, cend);
                patch_point(self.out, f.eoff, p.end);
            }
            w_u32_at(self.out, f.coff, f.count);
        }
    }
}

/// Append a JSON string literal (RFC 8259 escaping).
fn json_str(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str("\\u");
                for shift in [12, 8, 4, 0] {
                    let nib = (c as u32 >> shift) & 0xf;
                    out.push(char::from_digit(nib, 16).unwrap());
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// `"key":` helper.
fn key(k: &str, out: &mut String) {
    out.push('"');
    out.push_str(k);
    out.push_str("\":");
}

fn json_children(c: &[Mdast], out: &mut String) {
    out.push('[');
    for (i, child) in c.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        write_json(child, out);
    }
    out.push(']');
}

fn json_opt(v: &Option<String>, out: &mut String) {
    match v {
        Some(s) => json_str(s, out),
        None => out.push_str("null"),
    }
}

/// Write a directive `attributes` object `{"k":"v",…}` in insertion order.
#[cfg(feature = "directives")]
fn json_attrs(attrs: &[(String, String)], out: &mut String) {
    out.push('{');
    for (i, (k, v)) in attrs.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        json_str(k, out);
        out.push(':');
        json_str(v, out);
    }
    out.push('}');
}

fn write_point(p: (u32, u32, u32), out: &mut String) {
    out.push_str("{\"line\":");
    out.push_str(&p.0.to_string());
    out.push_str(",\"column\":");
    out.push_str(&p.1.to_string());
    out.push_str(",\"offset\":");
    out.push_str(&p.2.to_string());
    out.push('}');
}

fn write_json(node: &Mdast, out: &mut String) {
    write_json_pos(node, None, out)
}

/// `pos` carries a wrapping [`Mdast::Positioned`]'s position down to the inner
/// node so it appears as a `"position"` field on that node's object.
fn write_json_pos(node: &Mdast, pos: Option<&Pos>, out: &mut String) {
    if let Mdast::Positioned(p, inner) = node {
        write_json_pos(inner, Some(p), out);
        return;
    }
    out.push('{');
    key("type", out);
    match node {
        Mdast::Positioned(..) => unreachable!("handled above"),
        Mdast::Root(c) => {
            out.push_str("\"root\",");
            key("children", out);
            json_children(c, out);
        }
        Mdast::Paragraph(c) => {
            out.push_str("\"paragraph\",");
            key("children", out);
            json_children(c, out);
        }
        Mdast::Heading { depth, children } => {
            out.push_str("\"heading\",");
            key("depth", out);
            out.push((b'0' + depth) as char);
            out.push(',');
            key("children", out);
            json_children(children, out);
        }
        Mdast::Blockquote(c) => {
            out.push_str("\"blockquote\",");
            key("children", out);
            json_children(c, out);
        }
        Mdast::List {
            ordered,
            start,
            spread,
            children,
        } => {
            out.push_str("\"list\",");
            key("ordered", out);
            out.push_str(if *ordered { "true," } else { "false," });
            key("start", out);
            match start {
                Some(s) => out.push_str(&s.to_string()),
                None => out.push_str("null"),
            }
            out.push(',');
            key("spread", out);
            out.push_str(if *spread { "true," } else { "false," });
            key("children", out);
            json_children(children, out);
        }
        Mdast::ListItem { spread, children } => {
            out.push_str("\"listItem\",");
            key("spread", out);
            out.push_str(if *spread { "true," } else { "false," });
            key("checked", out);
            out.push_str("null,");
            key("children", out);
            json_children(children, out);
        }
        Mdast::ThematicBreak => out.push_str("\"thematicBreak\""),
        Mdast::Code { lang, meta, value } => {
            out.push_str("\"code\",");
            key("lang", out);
            json_opt(lang, out);
            out.push(',');
            key("meta", out);
            json_opt(meta, out);
            out.push(',');
            key("value", out);
            json_str(value, out);
        }
        Mdast::Html(v) => {
            out.push_str("\"html\",");
            key("value", out);
            json_str(v, out);
        }
        #[cfg(feature = "frontmatter")]
        Mdast::Yaml(v) => {
            out.push_str("\"yaml\",");
            key("value", out);
            json_str(v, out);
        }
        #[cfg(feature = "frontmatter")]
        Mdast::Toml(v) => {
            out.push_str("\"toml\",");
            key("value", out);
            json_str(v, out);
        }
        #[cfg(feature = "footnotes")]
        Mdast::FootnoteDefinition {
            identifier,
            label,
            children,
        } => {
            out.push_str("\"footnoteDefinition\",");
            key("identifier", out);
            json_str(identifier, out);
            out.push(',');
            key("label", out);
            json_str(label, out);
            out.push(',');
            key("children", out);
            json_children(children, out);
        }
        #[cfg(feature = "deflist")]
        Mdast::DefList(c) => {
            out.push_str("\"defList\",");
            key("children", out);
            json_children(c, out);
        }
        #[cfg(feature = "deflist")]
        Mdast::DefListTerm(c) => {
            out.push_str("\"defListTerm\",");
            key("children", out);
            json_children(c, out);
        }
        #[cfg(feature = "deflist")]
        Mdast::DefListDescription { spread, children } => {
            out.push_str("\"defListDescription\",");
            key("spread", out);
            out.push_str(if *spread { "true," } else { "false," });
            key("children", out);
            json_children(children, out);
        }
        #[cfg(feature = "directives")]
        Mdast::ContainerDirective {
            name,
            attributes,
            children,
        } => {
            out.push_str("\"containerDirective\",");
            key("name", out);
            json_str(name, out);
            out.push(',');
            key("attributes", out);
            json_attrs(attributes, out);
            out.push(',');
            key("children", out);
            json_children(children, out);
        }
        #[cfg(feature = "directives")]
        Mdast::LeafDirective {
            name,
            attributes,
            children,
        } => {
            out.push_str("\"leafDirective\",");
            key("name", out);
            json_str(name, out);
            out.push(',');
            key("attributes", out);
            json_attrs(attributes, out);
            out.push(',');
            key("children", out);
            json_children(children, out);
        }
        #[cfg(feature = "directives")]
        Mdast::TextDirective {
            name,
            attributes,
            children,
        } => {
            out.push_str("\"textDirective\",");
            key("name", out);
            json_str(name, out);
            out.push(',');
            key("attributes", out);
            json_attrs(attributes, out);
            out.push(',');
            key("children", out);
            json_children(children, out);
        }
        #[cfg(feature = "directives")]
        Mdast::DirectiveLabel(children) => {
            out.push_str("\"paragraph\",");
            key("data", out);
            out.push_str("{\"directiveLabel\":true},");
            key("children", out);
            json_children(children, out);
        }
        #[cfg(feature = "footnotes")]
        Mdast::FootnoteReference { identifier, label } => {
            out.push_str("\"footnoteReference\",");
            key("identifier", out);
            json_str(identifier, out);
            out.push(',');
            key("label", out);
            json_str(label, out);
        }
        Mdast::Text(v) => {
            out.push_str("\"text\",");
            key("value", out);
            json_str(v, out);
        }
        Mdast::Emphasis(c) => {
            out.push_str("\"emphasis\",");
            key("children", out);
            json_children(c, out);
        }
        Mdast::Strong(c) => {
            out.push_str("\"strong\",");
            key("children", out);
            json_children(c, out);
        }
        Mdast::Delete(c) => {
            out.push_str("\"delete\",");
            key("children", out);
            json_children(c, out);
        }
        Mdast::InlineCode(v) => {
            out.push_str("\"inlineCode\",");
            key("value", out);
            json_str(v, out);
        }
        Mdast::Break => out.push_str("\"break\""),
        Mdast::Link {
            url,
            title,
            children,
        } => {
            out.push_str("\"link\",");
            key("url", out);
            json_str(url, out);
            out.push(',');
            key("title", out);
            json_opt(title, out);
            out.push(',');
            key("children", out);
            json_children(children, out);
        }
        Mdast::Image { url, title, alt } => {
            out.push_str("\"image\",");
            key("url", out);
            json_str(url, out);
            out.push(',');
            key("title", out);
            json_opt(title, out);
            out.push(',');
            key("alt", out);
            json_str(alt, out);
        }
        Mdast::Definition {
            identifier,
            label,
            url,
            title,
        } => {
            out.push_str("\"definition\",");
            key("identifier", out);
            json_str(identifier, out);
            out.push(',');
            key("label", out);
            json_str(label, out);
            out.push(',');
            key("url", out);
            json_str(url, out);
            out.push(',');
            key("title", out);
            json_opt(title, out);
        }
        Mdast::LinkReference {
            identifier,
            label,
            reftype,
            children,
        } => {
            out.push_str("\"linkReference\",");
            key("identifier", out);
            json_str(identifier, out);
            out.push(',');
            key("label", out);
            json_str(label, out);
            out.push(',');
            key("referenceType", out);
            json_str(reftype, out);
            out.push(',');
            key("children", out);
            json_children(children, out);
        }
        Mdast::ImageReference {
            identifier,
            label,
            reftype,
            alt,
        } => {
            out.push_str("\"imageReference\",");
            key("identifier", out);
            json_str(identifier, out);
            out.push(',');
            key("label", out);
            json_str(label, out);
            out.push(',');
            key("referenceType", out);
            json_str(reftype, out);
            out.push(',');
            key("alt", out);
            json_str(alt, out);
        }
    }
    if let Some(p) = pos {
        out.push(',');
        key("position", out);
        out.push('{');
        out.push_str("\"start\":");
        write_point(p.start, out);
        out.push_str(",\"end\":");
        write_point(p.end, out);
        out.push('}');
    }
    out.push('}');
}

/// SPIKE diagnostic: total node count of the tree.
pub fn node_count(node: &Mdast) -> usize {
    use Mdast::*;
    let kids = |c: &[Mdast]| c.iter().map(node_count).sum::<usize>();
    match node {
        Root(c) | Paragraph(c) | Blockquote(c) | Emphasis(c) | Strong(c) | Delete(c) => 1 + kids(c),
        #[cfg(feature = "directives")]
        DirectiveLabel(c) => 1 + kids(c),
        #[cfg(feature = "deflist")]
        DefList(c) | DefListTerm(c) => 1 + kids(c),
        #[cfg(feature = "deflist")]
        DefListDescription { children, .. } => 1 + kids(children),
        #[cfg(feature = "footnotes")]
        FootnoteDefinition { children, .. } => 1 + kids(children),
        #[cfg(feature = "directives")]
        ContainerDirective { children, .. }
        | LeafDirective { children, .. }
        | TextDirective { children, .. } => 1 + kids(children),
        Heading { children, .. }
        | List { children, .. }
        | ListItem { children, .. }
        | Link { children, .. }
        | LinkReference { children, .. } => 1 + kids(children),
        #[cfg(feature = "footnotes")]
        FootnoteReference { .. } => 1,
        #[cfg(feature = "frontmatter")]
        Yaml(_) | Toml(_) => 1,
        ThematicBreak
        | Code { .. }
        | Definition { .. }
        | Html(_)
        | Text(_)
        | InlineCode(_)
        | Break
        | Image { .. }
        | ImageReference { .. } => 1,
        Positioned(_, inner) => node_count(inner),
    }
}

#[cfg(test)]
mod wire_nopos_tests {
    //! Verify the no-position wire (`to_mdast_wire_nopos_opts`) carries the exact
    //! same payload as the position wire (`to_mdast_wire_opts`) minus the per-node
    //! `6×u32` position points — and that the position-on wire is unchanged.
    //!
    //! A single schema walker decodes BOTH wires into a normalized byte stream that
    //! copies every non-position byte verbatim and, when `has_pos`, skips the 24
    //! position bytes that sit right after each node's tag. If the two normalized
    //! streams match, the only difference between the wires is the position bytes.

    use super::*;

    struct Reader<'a> {
        b: &'a [u8],
        p: usize,
        has_pos: bool,
        norm: Vec<u8>, // position-stripped output
    }

    impl<'a> Reader<'a> {
        fn u8(&mut self) -> u8 {
            let v = self.b[self.p];
            self.p += 1;
            self.norm.push(v);
            v
        }
        fn raw_u8(&mut self) -> u8 {
            // tag byte: consume but DON'T push yet (caller controls ordering)
            let v = self.b[self.p];
            self.p += 1;
            v
        }
        fn u32(&mut self) -> u32 {
            let v = u32::from_le_bytes(self.b[self.p..self.p + 4].try_into().unwrap());
            self.norm.extend_from_slice(&self.b[self.p..self.p + 4]);
            self.p += 4;
            v
        }
        fn bytes(&mut self, n: usize) {
            self.norm.extend_from_slice(&self.b[self.p..self.p + n]);
            self.p += n;
        }
        fn skip_pos(&mut self) {
            // Two points = 24 bytes, present only on the position wire. NOT pushed
            // to `norm` (this is exactly the byte difference we are proving).
            if self.has_pos {
                self.p += 24;
            }
        }
        fn wstr(&mut self) {
            let hdr = u32::from_le_bytes(self.b[self.p..self.p + 4].try_into().unwrap());
            self.norm.extend_from_slice(&self.b[self.p..self.p + 4]);
            self.p += 4;
            let n = (hdr & 0x7fff_ffff) as usize;
            self.bytes(n);
        }
        fn wopt(&mut self) {
            let hdr = u32::from_le_bytes(self.b[self.p..self.p + 4].try_into().unwrap());
            if hdr == u32::MAX {
                self.u32();
            } else {
                self.wstr();
            }
        }
        fn wattrs(&mut self) {
            let n = self.u32();
            for _ in 0..n {
                self.wstr();
                self.wstr();
            }
        }
        fn kids(&mut self) {
            let n = self.u32();
            for _ in 0..n {
                self.node();
            }
        }
        fn node(&mut self) {
            let tag = self.raw_u8();
            self.norm.push(tag);
            self.skip_pos();
            match tag {
                0 | 1 | 3 => self.kids(), // root, paragraph, blockquote
                2 => {
                    self.u8(); // depth
                    self.kids();
                }
                4 => {
                    self.u8(); // flags
                    self.u32(); // start
                    self.kids();
                }
                5 => {
                    self.u8(); // spread
                    self.u8(); // checked (0=false, 1=true, 2=none)
                    self.kids();
                }
                6 | 14 => {} // thematicBreak, break
                7 => {
                    self.wopt(); // lang
                    self.wopt(); // meta
                    self.wstr(); // value
                }
                8 | 9 | 13 => self.wstr(), // html, text, inlineCode
                10..=12 => self.kids(),    // emphasis, strong, delete
                15 => {
                    // link OR autolink. Disambiguate: a link container emits
                    // url,opt(title),kids; the autolink leaf emits
                    // url,opt(title=None),u32(1),text-child. Both start url+opt.
                    // The autolink path is distinguished structurally only by its
                    // inline child; since open/leaf both use tag 15, decode as a
                    // container (url,opt,kids) — autolink emits exactly one text
                    // child via kids()-compatible layout.
                    self.wstr(); // url
                    self.wopt(); // title
                    self.kids(); // children (autolink: count=1 + one text node)
                }
                16 => {
                    self.wstr(); // url
                    self.wopt(); // title
                    self.wstr(); // alt
                }
                17 => {
                    self.wstr(); // identifier
                    self.wstr(); // label
                    self.wstr(); // url
                    self.wopt(); // title
                }
                18 => {
                    self.wstr(); // identifier
                    self.wstr(); // label
                    self.u8(); // reftype
                    self.kids();
                }
                19 => {
                    self.wstr(); // identifier
                    self.wstr(); // label
                    self.u8(); // reftype
                    self.wstr(); // alt
                }
                20 | 21 => self.wstr(), // yaml, toml
                22 => {
                    self.wstr(); // identifier
                    self.wstr(); // label
                    self.kids();
                }
                23 => {
                    self.wstr(); // identifier
                    self.wstr(); // label
                }
                24 => self.kids(), // defList
                25 => self.kids(), // defListTerm
                26 => {
                    self.u8(); // spread
                    self.kids();
                }
                27 => {
                    self.wstr(); // name
                    self.wattrs();
                    self.kids(); // empty children on wire
                }
                28 => {
                    self.wstr(); // name
                    self.wattrs();
                    self.kids();
                }
                29 => {
                    self.wstr(); // name
                    self.wattrs();
                    self.kids();
                }
                30 => self.kids(), // directiveLabel paragraph
                other => panic!("unknown wire tag {other} at byte {}", self.p - 1),
            }
        }
    }

    /// Decode a wire into its position-stripped normalized byte stream.
    fn normalize(wire: &[u8], has_pos: bool) -> Vec<u8> {
        let mut r = Reader {
            b: wire,
            p: 0,
            has_pos,
            norm: Vec::with_capacity(wire.len()),
        };
        r.node();
        assert_eq!(r.p, wire.len(), "decoder did not consume the whole wire");
        r.norm
    }

    const SAMPLES: &[&str] = &[
        "",
        "hello world\n",
        "# Heading *em* and **strong** and `code`\n",
        "> a quote\n> over lines\n",
        "- one\n- two\n  - nested\n\n1. a\n2. b\n",
        "```rust\nfn main() {}\n```\n",
        "    indented code\n    line two\n",
        "para with [a link](https://x.com \"t\") and ![img](u.png \"a\")\n",
        "ref [link][id] and [id]: https://x.com \"title\"\n\n[id]: https://x.com\n",
        "an <https://auto.link> autolink\n",
        "<div>raw html</div>\n",
        "***\n\nmore\n",
        "line one\\\nline two with hard break\n",
        "unicode café — résumé 日本語 text\n",
        "a `multi\nword` and ~~del~~ mix [shortcut] ![imgref][]\n",
    ];

    #[test]
    fn nopos_equals_pos_minus_position() {
        for &src in SAMPLES {
            let opts = crate::Options::default();
            let pos = to_mdast_wire_opts(src, opts);
            let nopos = to_mdast_wire_nopos_opts(src, opts);
            // No-position wire must be strictly smaller (unless the doc is empty
            // of nodes, but root always exists -> 24 bytes saved minimum).
            assert!(
                nopos.len() < pos.len(),
                "nopos should drop position bytes for {src:?}: {} vs {}",
                nopos.len(),
                pos.len()
            );
            // The only difference must be the per-node position bytes.
            let a = normalize(&pos, true);
            let b = normalize(&nopos, false);
            assert_eq!(a, b, "normalized payloads differ for {src:?}");
            // The bytes dropped == 24 per node.
            assert_eq!(
                (pos.len() - nopos.len()) % 24,
                0,
                "dropped byte count not a multiple of 24 for {src:?}"
            );
        }
    }

    #[test]
    fn pos_wire_is_deterministic() {
        // Sanity: the position-on wire is stable across calls (guards added by the
        // nopos work must not perturb the enabled path).
        for &src in SAMPLES {
            let a = to_mdast_wire(src);
            let b = to_mdast_wire(src);
            assert_eq!(a, b, "pos wire not deterministic for {src:?}");
        }
    }
}

#[cfg(test)]
mod wire_pooled_tests {
    //! Verify the string-pooled wire (`to_mdast_wire_fast_opts`) reproduces exactly
    //! the same strings, in the same DFS order, as the inline no-position wire
    //! (`to_mdast_wire_nopos_opts`). Two schema walkers — identical structurally —
    //! decode both wires into the ordered list of strings: the inline walker reads
    //! `[u32 hdr][bytes]`; the pooled walker reads `[u32 u16Len]` and slices the
    //! pool at a running UTF-16 offset. If the two string lists are byte-equal, the
    //! pool (sliced by UTF-16 offset) reproduces the inline strings exactly.

    use super::*;

    /// A walker over the no-position structure. `read_str`/`read_opt` push the
    /// decoded string bytes onto `out`. The tag dispatch mirrors the canonical
    /// `wire_nopos_tests::Reader` schema exactly (no position bytes on either wire).
    struct StrWalker<'a, F, G> {
        b: &'a [u8],
        p: usize,
        out: Vec<Vec<u8>>,
        read_str: F,
        read_opt: G,
    }

    impl<F, G> StrWalker<'_, F, G>
    where
        F: FnMut(&[u8], &mut usize) -> Vec<u8>,
        G: FnMut(&[u8], &mut usize) -> Option<Vec<u8>>,
    {
        fn u8(&mut self) -> u8 {
            let v = self.b[self.p];
            self.p += 1;
            v
        }
        fn u32(&mut self) -> u32 {
            let v = u32::from_le_bytes(self.b[self.p..self.p + 4].try_into().unwrap());
            self.p += 4;
            v
        }
        fn wstr(&mut self) {
            let s = (self.read_str)(self.b, &mut self.p);
            self.out.push(s);
        }
        fn wopt(&mut self) {
            if let Some(s) = (self.read_opt)(self.b, &mut self.p) {
                self.out.push(s);
            }
        }
        fn wattrs(&mut self) {
            let n = self.u32();
            for _ in 0..n {
                self.wstr();
                self.wstr();
            }
        }
        fn kids(&mut self) {
            let n = self.u32();
            for _ in 0..n {
                self.node();
            }
        }
        fn node(&mut self) {
            let tag = self.u8();
            match tag {
                0 | 1 | 3 => self.kids(),
                2 => {
                    self.u8();
                    self.kids();
                }
                4 => {
                    self.u8();
                    self.u32();
                    self.kids();
                }
                5 => {
                    self.u8(); // spread
                    self.u8(); // checked
                    self.kids();
                }
                6 | 14 => {}
                7 => {
                    self.wopt();
                    self.wopt();
                    self.wstr();
                }
                8 | 9 | 13 => self.wstr(),
                10..=12 => self.kids(),
                15 => {
                    self.wstr();
                    self.wopt();
                    self.kids();
                }
                16 => {
                    self.wstr();
                    self.wopt();
                    self.wstr();
                }
                17 => {
                    self.wstr();
                    self.wstr();
                    self.wstr();
                    self.wopt();
                }
                18 => {
                    self.wstr();
                    self.wstr();
                    self.u8();
                    self.kids();
                }
                19 => {
                    self.wstr();
                    self.wstr();
                    self.u8();
                    self.wstr();
                }
                20 | 21 => self.wstr(),
                22 => {
                    self.wstr();
                    self.wstr();
                    self.kids();
                }
                23 => {
                    self.wstr();
                    self.wstr();
                }
                24 => self.kids(),
                25 => self.kids(),
                26 => {
                    self.u8();
                    self.kids();
                }
                27..=29 => {
                    self.wstr();
                    self.wattrs();
                    self.kids();
                }
                30 => self.kids(),
                other => panic!("unknown wire tag {other} at byte {}", self.p - 1),
            }
        }
    }

    /// Strings (in DFS order) from the inline no-position wire: `[u32 hdr][bytes]`,
    /// where `hdr`'s low 31 bits are the byte length and `None` is `0xFFFFFFFF`.
    fn inline_strings(wire: &[u8]) -> Vec<Vec<u8>> {
        let read_str = |b: &[u8], p: &mut usize| -> Vec<u8> {
            let hdr = u32::from_le_bytes(b[*p..*p + 4].try_into().unwrap());
            *p += 4;
            let n = (hdr & 0x7fff_ffff) as usize;
            let s = b[*p..*p + n].to_vec();
            *p += n;
            s
        };
        let read_opt = |b: &[u8], p: &mut usize| -> Option<Vec<u8>> {
            let hdr = u32::from_le_bytes(b[*p..*p + 4].try_into().unwrap());
            if hdr == u32::MAX {
                *p += 4;
                None
            } else {
                let n = (hdr & 0x7fff_ffff) as usize;
                *p += 4;
                let s = b[*p..*p + n].to_vec();
                *p += n;
                Some(s)
            }
        };
        let mut w = StrWalker {
            b: wire,
            p: 0,
            out: Vec::new(),
            read_str,
            read_opt,
        };
        w.node();
        assert_eq!(w.p, wire.len(), "inline decoder did not consume whole wire");
        w.out
    }

    /// Strings (in DFS order) from the pooled wire `[u32 poolStart][structure][pool]`:
    /// the structure holds `[u32 u16Len]` per string (`0xFFFFFFFF` for `None`); a
    /// running UTF-16 offset slices the pool. Mirrors the JS reader.
    fn pooled_strings(wire: &[u8]) -> Vec<Vec<u8>> {
        let pool_start = u32::from_le_bytes(wire[0..4].try_into().unwrap()) as usize;
        let structure = &wire[4..pool_start];
        // Decode the pool ONCE (like JS `TextDecoder.decode`) into UTF-16 units, so
        // a `[u16off, u16off + u16Len)` slice recovers the exact source string —
        // matching the JS reader's `pool.slice(off, off + len)`.
        let pool_str = std::str::from_utf8(&wire[pool_start..]).expect("pool is valid utf-8");
        let pool16: Vec<u16> = pool_str.encode_utf16().collect();
        let off = std::cell::Cell::new(0usize);
        let slice_pool = |u16_len: usize| -> Vec<u8> {
            let start = off.get();
            let end = start + u16_len;
            off.set(end);
            String::from_utf16(&pool16[start..end])
                .expect("pool slice is a valid utf-16 run")
                .into_bytes()
        };
        let read_str = |b: &[u8], p: &mut usize| -> Vec<u8> {
            let len = u32::from_le_bytes(b[*p..*p + 4].try_into().unwrap()) as usize;
            *p += 4;
            slice_pool(len)
        };
        let read_opt = |b: &[u8], p: &mut usize| -> Option<Vec<u8>> {
            let hdr = u32::from_le_bytes(b[*p..*p + 4].try_into().unwrap());
            *p += 4;
            if hdr == u32::MAX {
                None
            } else {
                Some(slice_pool(hdr as usize))
            }
        };
        let mut w = StrWalker {
            b: structure,
            p: 0,
            out: Vec::new(),
            read_str,
            read_opt,
        };
        w.node();
        assert_eq!(
            w.p,
            structure.len(),
            "pooled decoder did not consume whole structure"
        );
        // Every pool byte must be consumed by the running offset.
        assert_eq!(off.get(), pool16.len(), "pool not fully consumed");
        w.out
    }

    const SAMPLES: &[&str] = &[
        "",
        "hello world\n",
        "# Heading *em* and **strong** and `code`\n",
        "> a quote\n> over lines\n",
        "- one\n- two\n  - nested\n\n1. a\n2. b\n",
        "```rust\nfn main() {}\n```\n",
        "    indented code\n    line two\n",
        "para with [a link](https://x.com \"t\") and ![img](u.png \"a\")\n",
        "ref [link][id] and [id]: https://x.com \"title\"\n\n[id]: https://x.com\n",
        "an <https://auto.link> autolink\n",
        "<div>raw html</div>\n",
        "***\n\nmore\n",
        "line one\\\nline two with hard break\n",
        "unicode café — résumé 日本語 text\n",
        "föö 🎉 café and **bold 🚀** plus `code ✓`\n",
        "a `multi\nword` and ~~del~~ mix [shortcut] ![imgref][]\n",
        "empty link [](#) and image ![]()\n",
    ];

    #[test]
    fn pooled_strings_equal_inline_strings() {
        for &src in SAMPLES {
            let opts = crate::Options::default();
            let inline = to_mdast_wire_nopos_opts(src, opts);
            let pooled = to_mdast_wire_fast_opts(src, opts);

            // Layout sanity: poolStart points just past the structure.
            let pool_start = u32::from_le_bytes(pooled[0..4].try_into().unwrap()) as usize;
            assert!(
                pool_start <= pooled.len() && pool_start >= 4,
                "poolStart out of range for {src:?}: {pool_start} / {}",
                pooled.len()
            );

            let a = inline_strings(&inline);
            let b = pooled_strings(&pooled);
            assert_eq!(
                a, b,
                "pooled strings (sliced by UTF-16 offset) differ from inline for {src:?}"
            );
        }
    }

    #[test]
    fn pooled_wire_is_deterministic() {
        for &src in SAMPLES {
            let a = to_mdast_wire_fast_opts(src, crate::Options::default());
            let b = to_mdast_wire_fast_opts(src, crate::Options::default());
            assert_eq!(a, b, "pooled wire not deterministic for {src:?}");
        }
    }

    /// The reused [`WireFast`] context must produce byte-identical output to the
    /// standalone [`to_mdast_wire_fast_opts`] across many *different* docs driven
    /// through ONE context — a missed clear leaks stale scratch state between
    /// parses. Interleaving footnote-on and footnote-off docs is the critical case
    /// for the reused `Scratch.footnote_ids`.
    #[test]
    fn wirefast_reuse_byte_identical() {
        let mut wf = WireFast::new();
        let opt_sets = [
            crate::Options::default(),
            crate::Options {
                footnotes: true,
                ..crate::Options::default()
            },
        ];
        // A footnote doc (seeds footnote_ids) plus plain docs, interleaved so a
        // stale id set from a previous footnote parse would surface.
        let fn_doc = "a ref[^x] and [^y]\n\n[^x]: def x\n[^y]: def y\n";
        for round in 0..3 {
            for &opts in &opt_sets {
                let docs: &[&str] = &[fn_doc];
                for &src in docs.iter().chain(SAMPLES) {
                    let want = to_mdast_wire_fast_opts(src, opts);
                    let got = wf.emit(src, opts);
                    assert_eq!(
                        want, got,
                        "WireFast reuse diverged (round {round}, footnotes={}) for {src:?}",
                        opts.footnotes
                    );
                }
            }
        }
    }
}
