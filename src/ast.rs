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
    Yaml(String),
    /// TOML frontmatter (`+++`); `value` is the text between the fences.
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
    ContainerDirective {
        name: String,
        attributes: Vec<(String, String)>,
        children: Vec<Mdast>,
    },
    /// remark-directive `leafDirective` (`::name`) — inline children (the label).
    LeafDirective {
        name: String,
        attributes: Vec<(String, String)>,
        children: Vec<Mdast>,
    },
    /// A container directive's `[label]`: serialized as a `paragraph` carrying
    /// `data: { directiveLabel: true }` (matching remark-directive).
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
    to_mdast_opts(src, crate::Options::default())
}

/// Like [`to_mdast`] but with opt-in grammar extensions (e.g. frontmatter).
pub fn to_mdast_opts(src: &str, opts: crate::Options) -> Mdast {
    let tree = crate::block::parse_with_opts(src, opts);
    let mut scratch = fn_scratch(&tree);
    let ctx = PosCtx::new(src);
    block(&tree, tree.root, &mut scratch, &ctx).0
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
                                kids.push(Mdast::Positioned(
                                    pos,
                                    Box::new(Mdast::DefListTerm(inl)),
                                ));
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
                        let para = Mdast::Positioned(pos.clone(), Box::new(Mdast::Paragraph(inl)));
                        kids.push(Mdast::Positioned(
                            pos,
                            Box::new(Mdast::DefListDescription {
                                spread,
                                children: vec![para],
                            }),
                        ));
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
            let para = Mdast::Positioned(
                ctx.pos(sb, eb),
                Box::new(Mdast::Paragraph(inline(tree, idx, scratch, ctx, sb, eb))),
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
        Kind::ContainerDirective => {
            let d = tree.directive(idx);
            // A `[label]` on the opener becomes a leading `paragraph` carrying
            // `data.directiveLabel` (matching remark-directive); the fenced body
            // parses as ordinary block children.
            let mut kids: Vec<Mdast> = Vec::new();
            if let Some((ls, le)) = d.label {
                let label = directive_label_children(tree, d, scratch, ctx);
                // remark-directive spans the label paragraph over the brackets.
                let lpos = ctx.pos(ls as usize - 1, le as usize + 1);
                kids.push(Mdast::Positioned(
                    lpos,
                    Box::new(Mdast::DirectiveLabel(label)),
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
    (
        Mdast::Positioned(ctx.pos(start_b, end_b), Box::new(inner)),
        end_b,
    )
}

/// Build the inline children of a block directive's `[label]` (empty when absent).
/// The label is a contiguous source range, so child token offsets map directly.
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
    let bpos = ctx.pos(ls as usize, le as usize);
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
    let bpos = ctx.pos(sbb, ebb);
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
    let bpos = ctx.pos(sb, eb);
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
    content: &str,
    toks: Vec<SpanTok>,
    tree: &Tree,
    scratch: &mut Scratch,
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
                stack
                    .last_mut()
                    .unwrap()
                    .1
                    .push(Mdast::Positioned(p, Box::new(Mdast::InlineCode(v))));
            }
            InlineTok::Html(h) => {
                let p = mkpos(start, end);
                stack
                    .last_mut()
                    .unwrap()
                    .1
                    .push(Mdast::Positioned(p, Box::new(Mdast::Html(h))));
            }
            InlineTok::Break => {
                let p = mkpos(start, end);
                stack
                    .last_mut()
                    .unwrap()
                    .1
                    .push(Mdast::Positioned(p, Box::new(Mdast::Break)));
            }
            InlineTok::Image { url, title, alt } => {
                let p = mkpos(start, end);
                stack.last_mut().unwrap().1.push(Mdast::Positioned(
                    p,
                    Box::new(Mdast::Image { url, title, alt }),
                ));
            }
            InlineTok::ImageRef {
                identifier,
                label,
                reftype,
                alt,
            } => {
                let p = mkpos(start, end);
                stack.last_mut().unwrap().1.push(Mdast::Positioned(
                    p,
                    Box::new(Mdast::ImageReference {
                        identifier,
                        label,
                        reftype,
                        alt,
                    }),
                ));
            }
            #[cfg(feature = "footnotes")]
            InlineTok::FootnoteRef { identifier, label } => {
                let p = mkpos(start, end);
                stack.last_mut().unwrap().1.push(Mdast::Positioned(
                    p,
                    Box::new(Mdast::FootnoteReference { identifier, label }),
                ));
            }
            InlineTok::TextDirective { name, attrs, label } => {
                let p = mkpos(start, end);
                let children = match label {
                    Some((ls, le)) => {
                        let body = &content[ls as usize..le as usize];
                        let toks2 = render_inline_to_tokens(body, &tree.refmap, scratch, tree.opts);
                        let lpos = ctx.pos(map(ls), if le == 0 { map(0) } else { map(le - 1) + 1 });
                        build_inline(body, toks2, tree, scratch, ctx, &|o| map(ls + o), &lpos)
                    }
                    None => Vec::new(),
                };
                stack.last_mut().unwrap().1.push(Mdast::Positioned(
                    p,
                    Box::new(Mdast::TextDirective {
                        name,
                        attributes: attrs,
                        children,
                    }),
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
                    Box::new(Mdast::Link {
                        url,
                        title: None,
                        children: vec![child],
                    }),
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
                let p = mkpos(os, cend);
                stack
                    .last_mut()
                    .unwrap()
                    .1
                    .push(Mdast::Positioned(p, Box::new(node)));
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

#[inline]
fn w_u32(v: u32, out: &mut Vec<u8>) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn w_str(s: &str, out: &mut Vec<u8>) {
    // Encode the ASCII-ness in the length's high bit so the JS reader can pick
    // the fast `String.fromCharCode` path without re-scanning the bytes itself
    // (Rust's `is_ascii` is a cheap vectorized scan). Lengths are « 2^31.
    let hdr = (s.len() as u32) | if s.is_ascii() { 0x8000_0000 } else { 0 };
    w_u32(hdr, out);
    out.extend_from_slice(s.as_bytes());
}
fn w_opt(o: &Option<String>, out: &mut Vec<u8>) {
    match o {
        Some(s) => w_str(s, out),
        None => w_u32(u32::MAX, out),
    }
}
fn w_opt_str(o: Option<&str>, out: &mut Vec<u8>) {
    match o {
        Some(s) => w_str(s, out),
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
            w_point(out, ctx.point(0));
            w_point(out, ctx.point(eb));
            let coff = reserve(out, 4);
            let n = bchildren(tree, idx, scratch, ctx, out).0;
            w_u32_at(out, coff, n);
            eb
        }
        Kind::Paragraph => {
            let eb = ctx.rtrim_nl(se);
            out.push(1);
            w_point(out, ctx.point(sb));
            w_point(out, ctx.point(eb));
            let coff = reserve(out, 4);
            let n = inline_wire(tree, idx, scratch, ctx, sb, eb, out);
            w_u32_at(out, coff, n);
            eb
        }
        Kind::Heading => {
            let eb = ctx.rtrim_nl(se);
            out.push(2);
            w_point(out, ctx.point(sb));
            w_point(out, ctx.point(eb));
            out.push(node.level);
            let coff = reserve(out, 4);
            let n = inline_wire(tree, idx, scratch, ctx, sb, eb, out);
            w_u32_at(out, coff, n);
            eb
        }
        Kind::BlockQuote => {
            out.push(3);
            w_point(out, ctx.point(sb));
            let eoff = reserve(out, 12);
            let coff = reserve(out, 4);
            let (n, last) = bchildren(tree, idx, scratch, ctx, out);
            let end = last.map_or(se, |l| l.max(se));
            patch_point(out, eoff, ctx.point(end));
            w_u32_at(out, coff, n);
            end
        }
        Kind::List => {
            let ld = node.list.as_ref().unwrap();
            out.push(4);
            w_point(out, ctx.point(sb));
            let eoff = reserve(out, 12);
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
            patch_point(out, eoff, ctx.point(end));
            w_u32_at(out, coff, n);
            end
        }
        Kind::Item => {
            out.push(5);
            w_point(out, ctx.point(sb));
            let eoff = reserve(out, 12);
            out.push(node.item_spread as u8);
            let coff = reserve(out, 4);
            let (n, last) = bchildren(tree, idx, scratch, ctx, out);
            let end = last.unwrap_or(se);
            patch_point(out, eoff, ctx.point(end));
            w_u32_at(out, coff, n);
            end
        }
        Kind::ThematicBreak => {
            let eb = ctx.rtrim_nl(se);
            out.push(6);
            w_point(out, ctx.point(sb));
            w_point(out, ctx.point(eb));
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
            w_point(out, ctx.point(sb));
            w_point(out, ctx.point(eb));
            w_opt(&lang, out);
            w_opt(&meta, out);
            w_str(&value, out);
            eb
        }
        Kind::HtmlBlock => {
            let eb = tree.html_ast_end(idx) as usize;
            out.push(8);
            w_point(out, ctx.point(sb));
            w_point(out, ctx.point(eb));
            w_str(tree.html_value(idx), out);
            eb
        }
        Kind::Frontmatter => {
            let value = frontmatter_value(tree.content(idx));
            out.push(if node.level == 1 { 21 } else { 20 }); // 20 = yaml, 21 = toml
            w_point(out, ctx.point(sb));
            w_point(out, ctx.point(se));
            w_str(value, out);
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
            w_point(out, ctx.point(sb));
            let eoff = reserve(out, 12);
            w_str(&d.identifier, out);
            w_str(&d.label, out);
            let coff = reserve(out, 4);
            let (n, last) = bchildren(tree, idx, scratch, ctx, out);
            let end = last.unwrap_or(se);
            patch_point(out, eoff, ctx.point(end));
            w_u32_at(out, coff, n);
            end
        }
        Kind::Definition => {
            let d = tree.definition(idx);
            let eb = ctx.line_content_end(ctx.rtrim(sb, se));
            out.push(17);
            w_point(out, ctx.point(sb));
            w_point(out, ctx.point(eb));
            w_str(&d.identifier, out);
            w_str(&d.label, out);
            w_str(&d.url, out);
            w_opt(&d.title, out);
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
            w_point(out, ctx.point(sb));
            let eoff = reserve(out, 12);
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
                                w_point(out, ctx.point(sbb));
                                let teoff = reserve(out, 12);
                                let tcoff = reserve(out, 4);
                                let (ebb, tn) =
                                    inline_wire_slice(tree, ci, off, body, scratch, ctx, out);
                                patch_point(out, teoff, ctx.point(ebb));
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
                        w_point(out, ctx.point(sbb));
                        let deoff = reserve(out, 12);
                        out.push(spread as u8);
                        let dcoff = reserve(out, 4);
                        // Single child: a paragraph wrapping the inline body.
                        out.push(1); // paragraph
                        w_point(out, ctx.point(sbb));
                        let peoff = reserve(out, 12);
                        let pcoff = reserve(out, 4);
                        let (ebb, pn) = inline_wire_slice(tree, ci, off, body, scratch, ctx, out);
                        patch_point(out, peoff, ctx.point(ebb));
                        w_u32_at(out, pcoff, pn);
                        patch_point(out, deoff, ctx.point(ebb));
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
            patch_point(out, eoff, ctx.point(last));
            w_u32_at(out, coff, n);
            last
        }
        Kind::LeafDirective => {
            let d = tree.directive(idx);
            let eb = ctx.rtrim_nl(se);
            out.push(28); // leafDirective
            w_point(out, ctx.point(sb));
            w_point(out, ctx.point(eb));
            w_str(&d.name, out);
            w_attrs(&d.attrs, out);
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
        Kind::ContainerDirective => {
            let d = tree.directive(idx);
            out.push(29); // containerDirective
            w_point(out, ctx.point(sb));
            let eoff = reserve(out, 12);
            w_str(&d.name, out);
            w_attrs(&d.attrs, out);
            let coff = reserve(out, 4);
            let mut n = 0u32;
            let mut last = se;
            if let Some((ls, le)) = d.label {
                // directiveLabel paragraph (tag 30).
                out.push(30);
                w_point(out, ctx.point(ls as usize));
                let leoff = reserve(out, 12);
                let lcoff = reserve(out, 4);
                let (lend, ln) =
                    inline_wire_src(tree, ls, tree.source_range(ls, le), scratch, ctx, out);
                patch_point(out, leoff, ctx.point(lend));
                w_u32_at(out, lcoff, ln);
                n += 1;
            }
            let (bn, blast) = bchildren(tree, idx, scratch, ctx, out);
            n += bn;
            if let Some(l) = blast {
                last = last.max(l);
            }
            patch_point(out, eoff, ctx.point(last));
            w_u32_at(out, coff, n);
            last
        }
        // Built inline by their `DefList` parent; unreachable but kept exhaustive.
        #[cfg(feature = "deflist")]
        Kind::DefTerm => {
            let eb = ctx.rtrim_nl(se);
            out.push(25);
            w_point(out, ctx.point(sb));
            w_point(out, ctx.point(eb));
            let coff = reserve(out, 4);
            let n = inline_wire(tree, idx, scratch, ctx, sb, eb, out);
            w_u32_at(out, coff, n);
            eb
        }
        #[cfg(feature = "deflist")]
        Kind::DefDesc => {
            let eb = ctx.rtrim_nl(se);
            out.push(26);
            w_point(out, ctx.point(sb));
            w_point(out, ctx.point(eb));
            out.push((node.level == 1) as u8);
            w_u32(0, out);
            eb
        }
        #[cfg(feature = "gfm")]
        Kind::Table => {
            let eb = ctx.rtrim(sb, se);
            out.push(8);
            w_point(out, ctx.point(sb));
            w_point(out, ctx.point(eb));
            w_str(tree.content(idx), out);
            eb
        }
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
        bpos: ctx.pos(sb, eb),
        stack: Vec::new(),
        top: 0,
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
        bpos: ctx.pos(src_start as usize, end),
        stack: Vec::new(),
        top: 0,
    };
    render_inline_to_sink(body, &tree.refmap, scratch, tree.opts, &mut sink);
    (end, sink.top)
}

/// Write an ordered attribute object to wire: `u32` count, then `(key, value)`
/// string pairs.
fn w_attrs(attrs: &[(String, String)], out: &mut Vec<u8>) {
    w_u32(attrs.len() as u32, out);
    for (k, v) in attrs {
        w_str(k, out);
        w_str(v, out);
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
        bpos: ctx.pos(sbb, ebb),
        stack: Vec::new(),
        top: 0,
    };
    render_inline_to_sink(body, &tree.refmap, scratch, tree.opts, &mut sink);
    (ebb, sink.top)
}

/// An open inline container: byte offsets to backpatch (end position + child
/// count) plus the opener's content span and running child count.
struct InlineFrame {
    eoff: usize,
    coff: usize,
    os: u32,
    oe: u32,
    count: u32,
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
    /// Common leaf prologue: tag byte + full position.
    fn leaf(&mut self, tag: u8, start: u32, end: u32) {
        let p = self.mkpos(start, end);
        self.out.push(tag);
        w_point(self.out, p.start);
        w_point(self.out, p.end);
    }
}

impl InlineSink for WireSink<'_> {
    fn text(&mut self, value: &str, start: u32, end: u32) {
        self.leaf(9, start, end);
        w_str(value, self.out);
        self.bump();
    }
    fn code(&mut self, value: &str, start: u32, end: u32) {
        self.leaf(13, start, end);
        w_str(value, self.out);
        self.bump();
    }
    fn html(&mut self, value: &str, start: u32, end: u32) {
        self.leaf(8, start, end);
        w_str(value, self.out);
        self.bump();
    }
    fn brk(&mut self, start: u32, end: u32) {
        self.leaf(14, start, end);
        self.bump();
    }
    fn image(&mut self, url: &str, title: Option<&str>, alt: &str, start: u32, end: u32) {
        self.leaf(16, start, end);
        w_str(url, self.out);
        w_opt_str(title, self.out);
        w_str(alt, self.out);
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
        w_str(identifier, self.out);
        w_str(label, self.out);
        self.out.push(reftype_code(reftype));
        w_str(alt, self.out);
        self.bump();
    }
    #[cfg(feature = "footnotes")]
    fn footnote_ref(&mut self, identifier: &str, label: &str, start: u32, end: u32) {
        self.leaf(23, start, end);
        w_str(identifier, self.out);
        w_str(label, self.out);
        self.bump();
    }
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
        w_str(name, self.out);
        w_attrs(attrs, self.out);
        w_u32(0, self.out); // children: empty (best-effort on the wire path)
        self.bump();
    }
    fn autolink(&mut self, url: &str, text: &str, start: u32, end: u32) {
        self.leaf(15, start, end);
        w_str(url, self.out);
        w_u32(u32::MAX, self.out); // title: None
        w_u32(1, self.out); // one text child
        let cp = self.mkpos(start.saturating_add(1), end.saturating_sub(1));
        self.out.push(9);
        w_point(self.out, cp.start);
        w_point(self.out, cp.end);
        w_str(text, self.out);
        self.bump();
    }
    fn open(&mut self, kind: &'static str, start: u32, end: u32) {
        self.bump();
        let tag = match kind {
            "strong" => 11,
            "delete" => 12,
            _ => 10,
        };
        let ls = self.leaf_start(start);
        self.out.push(tag);
        w_point(self.out, ls);
        let eoff = reserve(self.out, 12);
        let coff = reserve(self.out, 4);
        self.stack.push(InlineFrame {
            eoff,
            coff,
            os: start,
            oe: end,
            count: 0,
        });
    }
    fn link_open(&mut self, url: &str, title: Option<&str>, start: u32, end: u32) {
        self.bump();
        let ls = self.leaf_start(start);
        self.out.push(15);
        w_point(self.out, ls);
        let eoff = reserve(self.out, 12);
        w_str(url, self.out);
        w_opt_str(title, self.out);
        let coff = reserve(self.out, 4);
        self.stack.push(InlineFrame {
            eoff,
            coff,
            os: start,
            oe: end,
            count: 0,
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
        let ls = self.leaf_start(start);
        self.out.push(18);
        w_point(self.out, ls);
        let eoff = reserve(self.out, 12);
        w_str(identifier, self.out);
        w_str(label, self.out);
        self.out.push(reftype_code(reftype));
        let coff = reserve(self.out, 4);
        self.stack.push(InlineFrame {
            eoff,
            coff,
            os: start,
            oe: end,
            count: 0,
        });
    }
    fn close(&mut self, _start: u32, end: u32) {
        if let Some(f) = self.stack.pop() {
            let cend = if end > f.oe { end } else { f.oe };
            let p = self.mkpos(f.os, cend);
            patch_point(self.out, f.eoff, p.end);
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
        Mdast::Yaml(v) => {
            out.push_str("\"yaml\",");
            key("value", out);
            json_str(v, out);
        }
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
        Root(c) | Paragraph(c) | Blockquote(c) | Emphasis(c) | Strong(c) | Delete(c)
        | DirectiveLabel(c) => 1 + kids(c),
        #[cfg(feature = "deflist")]
        DefList(c) | DefListTerm(c) => 1 + kids(c),
        #[cfg(feature = "deflist")]
        DefListDescription { children, .. } => 1 + kids(children),
        #[cfg(feature = "footnotes")]
        FootnoteDefinition { children, .. } => 1 + kids(children),
        Heading { children, .. }
        | List { children, .. }
        | ListItem { children, .. }
        | Link { children, .. }
        | ContainerDirective { children, .. }
        | LeafDirective { children, .. }
        | TextDirective { children, .. }
        | LinkReference { children, .. } => 1 + kids(children),
        #[cfg(feature = "footnotes")]
        FootnoteReference { .. } => 1,
        ThematicBreak
        | Code { .. }
        | Definition { .. }
        | Html(_)
        | Yaml(_)
        | Toml(_)
        | Text(_)
        | InlineCode(_)
        | Break
        | Image { .. }
        | ImageReference { .. } => 1,
        Positioned(_, inner) => node_count(inner),
    }
}
