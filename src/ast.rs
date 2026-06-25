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

use crate::block::{Kind, Tree, parse};
use crate::inline::{InlineTok, Scratch, render_inline_to_tokens};

/// An owned mdast node. Field names mirror the mdast spec so a thin serializer
/// (in the example) maps each to `{ type, ... }` verbatim.
#[derive(Debug, Clone)]
pub enum Mdast {
    // --- blocks ---
    Root(Vec<Mdast>),
    Paragraph(Vec<Mdast>),
    Heading { depth: u8, children: Vec<Mdast> },
    Blockquote(Vec<Mdast>),
    List { ordered: bool, start: Option<u64>, spread: bool, children: Vec<Mdast> },
    ListItem { spread: bool, children: Vec<Mdast> },
    ThematicBreak,
    Code { lang: Option<String>, meta: Option<String>, value: String },
    /// A link reference definition `[label]: url "title"`.
    Definition { identifier: String, label: String, url: String, title: Option<String> },
    /// Raw HTML — block (here) or inline (from the inline stream).
    Html(String),
    // --- inline ---
    Text(String),
    Emphasis(Vec<Mdast>),
    Strong(Vec<Mdast>),
    Delete(Vec<Mdast>),
    InlineCode(String),
    Break,
    Link { url: String, title: Option<String>, children: Vec<Mdast> },
    Image { url: String, title: Option<String>, alt: String },
    LinkReference { identifier: String, label: String, reftype: &'static str, children: Vec<Mdast> },
    ImageReference { identifier: String, label: String, reftype: &'static str, alt: String },
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

/// Source position context: byte offset of each line start, + source length.
struct PosCtx {
    line_off: Vec<usize>,
    src_len: usize,
    src: Vec<u8>,
}

impl PosCtx {
    fn new(src: &str) -> Self {
        let mut line_off = vec![0usize];
        for (i, &b) in src.as_bytes().iter().enumerate() {
            if b == b'\n' {
                line_off.push(i + 1);
            }
        }
        PosCtx { line_off, src_len: src.len(), src: src.as_bytes().to_vec() }
    }
    /// A `(line, column, offset)` point at a byte offset.
    fn point(&self, off: usize) -> (u32, u32, u32) {
        // Line = last line_off ≤ off.
        let line = self.line_off.partition_point(|&s| s <= off).max(1);
        let col = off - self.line_off[line - 1] + 1;
        (line as u32, col as u32, off as u32)
    }
    /// Block position from its 1-based start line. `end` covers the start line
    /// only (accurate for single-line blocks; an approximation for multi-line —
    /// documented in PLUGIN_FINDINGS.md). The whole-document root spans to EOF.
    fn block_pos(&self, start_line: u32, is_root: bool) -> Pos {
        if is_root {
            return Pos { start: (1, 1, 0), end: self.point(self.src_len) };
        }
        let l = start_line.max(1) as usize;
        let start_off = self.line_off[l - 1];
        let mut end_off = *self.line_off.get(l).unwrap_or(&self.src_len);
        if end_off > start_off && self.src.get(end_off - 1) == Some(&b'\n') {
            end_off -= 1;
        }
        Pos { start: self.point(start_off), end: self.point(end_off) }
    }
}

/// Parse `src` and build the nested mdast tree (CommonMark), with unist
/// `position` on block nodes.
pub fn to_mdast(src: &str) -> Mdast {
    let tree = parse(src);
    let mut scratch = Scratch::new();
    let ctx = PosCtx::new(src);
    block(&tree, tree.root, &mut scratch, &ctx)
}

fn block(tree: &Tree, idx: usize, scratch: &mut Scratch, ctx: &PosCtx) -> Mdast {
    let node = &tree.nodes[idx];
    let inner = match node.kind {
        Kind::Document => Mdast::Root(block_children(tree, idx, scratch, ctx)),
        Kind::Paragraph => Mdast::Paragraph(inline(tree, idx, scratch, ctx)),
        Kind::Heading => Mdast::Heading {
            depth: node.level,
            children: inline(tree, idx, scratch, ctx),
        },
        Kind::BlockQuote => Mdast::Blockquote(block_children(tree, idx, scratch, ctx)),
        Kind::ThematicBreak => Mdast::ThematicBreak,
        Kind::CodeBlock => {
            let (lang, meta) = code_info(tree.info(idx));
            // mdast `code.value` excludes the terminating newline.
            let mut value = tree.content(idx).to_owned();
            if value.ends_with('\n') {
                value.pop();
            }
            Mdast::Code { lang, meta, value }
        }
        // mdast keeps the html block's raw value (trailing-newline rule applied
        // in the block parser; differs from the HTML-render content).
        Kind::HtmlBlock => Mdast::Html(tree.html_value(idx).to_owned()),
        Kind::Definition => {
            let d = tree.definition(idx);
            Mdast::Definition {
                identifier: d.identifier.clone(),
                label: d.label.clone(),
                url: d.url.clone(),
                title: d.title.clone(),
            }
        }
        Kind::List => {
            let ld = node.list.as_ref().unwrap();
            Mdast::List {
                ordered: ld.ordered,
                start: ld.ordered.then_some(ld.start),
                // mdast `list.spread`: a blank line occurs *between items*.
                spread: ld.spread,
                children: block_children(tree, idx, scratch, ctx),
            }
        }
        Kind::Item => {
            // mdast `listItem.spread`: a blank line occurs *between this item's
            // own block children* (computed in the block parser's
            // `compute_spread`).
            Mdast::ListItem {
                spread: node.item_spread,
                children: block_children(tree, idx, scratch, ctx),
            }
        }
        // SPIKE scope is pure-CommonMark mdast. GFM tables would need
        // `table`/`tableRow`/`tableCell` nodes (remark-gfm's mdast extension) —
        // future work; for now degrade to a raw node so `ast,gfm` still builds.
        #[cfg(feature = "gfm")]
        Kind::Table => Mdast::Html(tree.content(idx).to_owned()),
    };
    let pos = ctx.block_pos(tree.start_line(idx), matches!(node.kind, Kind::Document));
    Mdast::Positioned(pos, Box::new(inner))
}

fn block_children(tree: &Tree, idx: usize, scratch: &mut Scratch, ctx: &PosCtx) -> Vec<Mdast> {
    let mut v = Vec::new();
    let mut c = tree.first_child(idx);
    while let Some(ci) = c {
        v.push(block(tree, ci, scratch, ctx));
        c = tree.next_sibling(ci);
    }
    v
}

/// Build a text-bearing block's inline children: capture the semantic token
/// stream, then fold `Open`/`Close`/`LinkOpen`/`LinkClose` into a nested tree.
///
/// Inline nodes get **block-granular** positions (the owning block's span):
/// coarse but valid, and enough for position-reading plugins (e.g. remark-lint,
/// which require *some* position on every node). Accurate per-inline offsets need
/// source spans threaded through the inline tokenizer — a documented next step.
fn inline(tree: &Tree, idx: usize, scratch: &mut Scratch, ctx: &PosCtx) -> Vec<Mdast> {
    let toks = render_inline_to_tokens(tree.content(idx), &tree.refmap, scratch, tree.opts);
    let mut nodes = build_inline(toks);
    let bpos = ctx.block_pos(tree.start_line(idx), false);
    position_inline(&mut nodes, &bpos);
    nodes
}

/// Wrap every inline node (and its descendants) in a `Positioned` with `pos`.
fn position_inline(nodes: &mut Vec<Mdast>, pos: &Pos) {
    for n in nodes.iter_mut() {
        match n {
            Mdast::Emphasis(c) | Mdast::Strong(c) | Mdast::Delete(c) => position_inline(c, pos),
            Mdast::Link { children, .. } | Mdast::LinkReference { children, .. } => {
                position_inline(children, pos)
            }
            _ => {}
        }
        let inner = std::mem::replace(n, Mdast::Break);
        *n = Mdast::Positioned(pos.clone(), Box::new(inner));
    }
}

/// An open inline container awaiting its close.
enum Frame {
    Container(&'static str), // "emphasis" | "strong" | "delete"
    Link { url: String, title: Option<String> },
    LinkRef { identifier: String, label: String, reftype: &'static str },
}

fn build_inline(toks: Vec<InlineTok>) -> Vec<Mdast> {
    // A stack of (frame, accumulated children). The bottom frame is the root.
    let mut stack: Vec<(Option<Frame>, Vec<Mdast>)> = vec![(None, Vec::new())];
    // Coalesce adjacent text nodes (mdast-util-from-markdown emits a single
    // `text` per run; matching that keeps the shape diff clean).
    let push = |stack: &mut Vec<(Option<Frame>, Vec<Mdast>)>, node: Mdast| {
        let siblings = &mut stack.last_mut().unwrap().1;
        if let (Mdast::Text(new), Some(Mdast::Text(prev))) = (&node, siblings.last_mut()) {
            prev.push_str(new);
        } else {
            siblings.push(node);
        }
    };
    for tok in toks {
        match tok {
            InlineTok::Text(s) => push(&mut stack, Mdast::Text(s)),
            InlineTok::Code(v) => push(&mut stack, Mdast::InlineCode(v)),
            InlineTok::Html(h) => push(&mut stack, Mdast::Html(h)),
            InlineTok::Break => push(&mut stack, Mdast::Break),
            InlineTok::Image { url, title, alt } => push(&mut stack, Mdast::Image { url, title, alt }),
            InlineTok::ImageRef { identifier, label, reftype, alt } => {
                push(&mut stack, Mdast::ImageReference { identifier, label, reftype, alt })
            }
            InlineTok::Autolink { url, text } => push(
                &mut stack,
                Mdast::Link { url, title: None, children: vec![Mdast::Text(text)] },
            ),
            InlineTok::Open(kind) => stack.push((Some(Frame::Container(kind)), Vec::new())),
            InlineTok::LinkOpen { url, title } => stack.push((Some(Frame::Link { url, title }), Vec::new())),
            InlineTok::LinkRefOpen { identifier, label, reftype } => {
                stack.push((Some(Frame::LinkRef { identifier, label, reftype }), Vec::new()))
            }
            InlineTok::Close(_) | InlineTok::LinkClose => {
                let (frame, children) = stack.pop().unwrap();
                let node = match frame {
                    Some(Frame::Container("strong")) => Mdast::Strong(children),
                    Some(Frame::Container("delete")) => Mdast::Delete(children),
                    Some(Frame::Container(_)) => Mdast::Emphasis(children),
                    Some(Frame::Link { url, title }) => Mdast::Link { url, title, children },
                    Some(Frame::LinkRef { identifier, label, reftype }) => {
                        Mdast::LinkReference { identifier, label, reftype, children }
                    }
                    None => {
                        // Unbalanced close (shouldn't happen): drop children inline.
                        for c in children {
                            push(&mut stack, c);
                        }
                        continue;
                    }
                };
                push(&mut stack, node);
            }
        }
    }
    // Any unclosed frames: flatten their children up (defensive).
    while stack.len() > 1 {
        let (_, children) = stack.pop().unwrap();
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
            (Some(lang.to_owned()), (!meta.is_empty()).then(|| meta.to_owned()))
        }
        None => (Some(trimmed.to_owned()), None),
    }
}

// ---- JSON serialization (zero-dep, for the wasm boundary) ----------------

/// SPIKE: parse `src` and serialize its mdast to a JSON string. Zero-dependency
/// (hand-rolled) so it works in the `wasm32-unknown-unknown` lib build. This is
/// what crosses the wasm→JS boundary in the boundary spike.
pub fn to_mdast_json(src: &str) -> String {
    let tree = to_mdast(src);
    // mdast JSON is roughly 3–5× the source; reserve generously to avoid regrows.
    let mut out = String::with_capacity(src.len() * 4 + 64);
    write_json(&tree, &mut out);
    out
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
        Mdast::List { ordered, start, spread, children } => {
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
        Mdast::Link { url, title, children } => {
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
        Mdast::Definition { identifier, label, url, title } => {
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
        Mdast::LinkReference { identifier, label, reftype, children } => {
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
        Mdast::ImageReference { identifier, label, reftype, alt } => {
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
        Heading { children, .. }
        | List { children, .. }
        | ListItem { children, .. }
        | Link { children, .. }
        | LinkReference { children, .. } => 1 + kids(children),
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
