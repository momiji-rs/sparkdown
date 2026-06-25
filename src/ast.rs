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
use crate::inline::{InlineTok, Scratch, SpanTok, render_inline_to_tokens};

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

/// Source position context: maps a source byte offset to a unist point. unist
/// `offset`/`column` are **UTF-16** units (JS string indices), so we precompute a
/// byte→UTF-16 prefix table alongside the line-start table.
struct PosCtx {
    line_off: Vec<usize>, // byte offset of each line start
    u16: Vec<u32>,        // u16[b] = UTF-16 units in src[0..b] (exact at char boundaries)
    src_len: usize,
    src: Vec<u8>,
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
        PosCtx { line_off, u16, src_len: src.len(), src: src.as_bytes().to_vec() }
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
        Pos { start: self.point(start), end: self.point(end) }
    }

    /// Drop trailing whitespace (spaces/tabs/line endings) from a byte range end.
    fn rtrim(&self, start: usize, mut end: usize) -> usize {
        while end > start && matches!(self.src[end - 1], b' ' | b'\t' | b'\n' | b'\r') {
            end -= 1;
        }
        end
    }

    /// Drop a single trailing line ending.
    fn rtrim_nl(&self, mut end: usize) -> usize {
        if end > 0 && self.src[end - 1] == b'\n' {
            end -= 1;
            if end > 0 && self.src[end - 1] == b'\r' {
                end -= 1;
            }
        }
        end
    }
}

/// Parse `src` and build the nested mdast tree (CommonMark), with accurate unist
/// `position` (UTF-16 line/column/offset) on every node.
pub fn to_mdast(src: &str) -> Mdast {
    let tree = parse(src);
    let mut scratch = Scratch::new();
    let ctx = PosCtx::new(src);
    block(&tree, tree.root, &mut scratch, &ctx).0
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
            (Mdast::Paragraph(inline(tree, idx, scratch, ctx, sb, eb)), sb, eb)
        }
        Kind::Heading => {
            // atx/setext span the whole line(s); the text child is trimmed.
            let eb = ctx.rtrim_nl(se);
            let depth = node.level;
            (
                Mdast::Heading { depth, children: inline(tree, idx, scratch, ctx, sb, eb) },
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
            // Fenced blocks already end at the closing fence; indented blocks end
            // after the last content line (trailing blanks excluded).
            let eb = if node.fenced { se } else { ctx.rtrim(sb, se) };
            (Mdast::Code { lang, meta, value }, sb, eb)
        }
        Kind::HtmlBlock => (Mdast::Html(tree.html_value(idx).to_owned()), sb, ctx.rtrim_nl(se)),
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
                ctx.rtrim(sb, se),
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
            (m, sb, last.unwrap_or(se))
        }
        Kind::Item => {
            let (kids, last) = block_children(tree, idx, scratch, ctx);
            (Mdast::ListItem { spread: node.item_spread, children: kids }, sb, last.unwrap_or(se))
        }
        #[cfg(feature = "gfm")]
        Kind::Table => (Mdast::Html(tree.content(idx).to_owned()), sb, ctx.rtrim(sb, se)),
    };
    (Mdast::Positioned(ctx.pos(start_b, end_b), Box::new(inner)), end_b)
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
    let bpos = ctx.pos(sb, eb);
    let map = |off: u32| tree.content_to_src(idx, off) as usize;
    build_inline(toks, ctx, &map, &bpos)
}

/// An open inline container awaiting its close.
enum Frame {
    Container(&'static str), // "emphasis" | "strong" | "delete"
    Link { url: String, title: Option<String> },
    LinkRef { identifier: String, label: String, reftype: &'static str },
}

/// Fold the [`SpanTok`] stream into a nested, positioned mdast. `base` is the
/// source byte offset of the inline content (so a content offset `o` maps to
/// source `base + o`); when `None` (buffered content) or a token span is unset,
/// the block-granular `bpos` is used.
fn build_inline(
    toks: Vec<SpanTok>,
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
    // (frame, children, open_start, open_end).
    let mut stack: Vec<(Option<Frame>, Vec<Mdast>, u32, u32)> =
        vec![(None, Vec::new(), u32::MAX, 0)];
    for SpanTok { tok, start, end } in toks {
        match tok {
            InlineTok::Text(s) => {
                let pos = mkpos(start, end);
                let sib = &mut stack.last_mut().unwrap().1;
                // Coalesce adjacent text (matching from-markdown's single run),
                // extending the merged node's end.
                if let Some(Mdast::Positioned(ppos, last)) = sib.last_mut()
                    && let Mdast::Text(prev) = last.as_mut()
                {
                    prev.push_str(&s);
                    ppos.end = pos.end;
                    continue;
                }
                sib.push(Mdast::Positioned(pos, Box::new(Mdast::Text(s))));
            }
            InlineTok::Code(v) => {
                let p = mkpos(start, end);
                stack.last_mut().unwrap().1.push(Mdast::Positioned(p, Box::new(Mdast::InlineCode(v))));
            }
            InlineTok::Html(h) => {
                let p = mkpos(start, end);
                stack.last_mut().unwrap().1.push(Mdast::Positioned(p, Box::new(Mdast::Html(h))));
            }
            InlineTok::Break => {
                let p = mkpos(start, end);
                stack.last_mut().unwrap().1.push(Mdast::Positioned(p, Box::new(Mdast::Break)));
            }
            InlineTok::Image { url, title, alt } => {
                let p = mkpos(start, end);
                stack.last_mut().unwrap().1.push(Mdast::Positioned(
                    p,
                    Box::new(Mdast::Image { url, title, alt }),
                ));
            }
            InlineTok::ImageRef { identifier, label, reftype, alt } => {
                let p = mkpos(start, end);
                stack.last_mut().unwrap().1.push(Mdast::Positioned(
                    p,
                    Box::new(Mdast::ImageReference { identifier, label, reftype, alt }),
                ));
            }
            InlineTok::Autolink { url, text } => {
                // The link spans `<url>`; its text child spans the url itself.
                let child = Mdast::Positioned(
                    mkpos(start.saturating_add(1), end.saturating_sub(1)),
                    Box::new(Mdast::Text(text)),
                );
                let p = mkpos(start, end);
                stack.last_mut().unwrap().1.push(Mdast::Positioned(
                    p,
                    Box::new(Mdast::Link { url, title: None, children: vec![child] }),
                ));
            }
            InlineTok::Open(kind) => {
                stack.push((Some(Frame::Container(kind)), Vec::new(), start, end))
            }
            InlineTok::LinkOpen { url, title } => {
                stack.push((Some(Frame::Link { url, title }), Vec::new(), start, end))
            }
            InlineTok::LinkRefOpen { identifier, label, reftype } => {
                stack.push((Some(Frame::LinkRef { identifier, label, reftype }), Vec::new(), start, end))
            }
            InlineTok::Close(_) | InlineTok::LinkClose => {
                let (frame, children, os, oe) = stack.pop().unwrap();
                let node = match frame {
                    Some(Frame::Container("strong")) => Mdast::Strong(children),
                    Some(Frame::Container("delete")) => Mdast::Delete(children),
                    Some(Frame::Container(_)) => Mdast::Emphasis(children),
                    Some(Frame::Link { url, title }) => Mdast::Link { url, title, children },
                    Some(Frame::LinkRef { identifier, label, reftype }) => {
                        Mdast::LinkReference { identifier, label, reftype, children }
                    }
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
                let p = mkpos(os, cend);
                stack.last_mut().unwrap().1.push(Mdast::Positioned(p, Box::new(node)));
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
