//! Block parser — container-aware and incremental (CommonMark §3–§5).
//!
//! A faithful port of the reference algorithm: each line is matched against
//! the open-block tree (continuation), then against block starts (new
//! containers/leaves), then its text is added to the open leaf. The result is
//! a node arena ([`Tree`]) that the renderer walks. Inline content is parsed
//! lazily at render time.

use crate::inline::{RefMap, take_ref_defs};
use crate::options::Options;
use crate::scan::memchr1;

const CODE_INDENT: usize = 4;

/// Sentinel for "no node" in the first-child/next-sibling links.
const NO_NODE: u32 = u32::MAX;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Document,
    BlockQuote,
    List,
    Item,
    Paragraph,
    Heading,
    ThematicBreak,
    CodeBlock,
    HtmlBlock,
    /// YAML (`---`) / TOML (`+++`) frontmatter at the very start of the document
    /// (opt-in via [`Options::frontmatter`]). Renders to nothing; `level` is 0
    /// for YAML, 1 for TOML. The content range covers the lines between the
    /// fences (used for the mdast `yaml`/`toml` node's `value`).
    Frontmatter,
    /// GFM footnote definition `[^label]: …` (opt-in via [`Options::footnotes`]).
    /// A block container (fixed 4-space content indent, lazy paragraph
    /// continuation) carrying its label via `Node::fn_idx` → [`Tree::fn_defs`].
    ///
    /// The variant stays compiled even without the `footnotes` feature (only its
    /// *producer* `start_footnote_def` and the heavy footnote code are cfg'd out,
    /// so no such node is ever created): removing a variant shrinks the hot
    /// `continue_block`/`can_contain` matches and perturbs their codegen (measured
    /// +2% on the default path). Keeping the variant + its small arms keeps those
    /// matches byte-identical to the proven-neutral baseline.
    FootnoteDef,
    /// Definition list container (`<dl>`), opt-in via [`Options::deflist`]. Holds
    /// an alternating sequence of [`Kind::DefTerm`] and [`Kind::DefDesc`] children.
    DefList,
    /// Definition-list term holder: a former [`Kind::Paragraph`] reinterpreted as
    /// the term(s) when a `: definition` line follows it. Its content may span
    /// several lines; each line renders as one `<dt>`.
    DefTerm,
    /// One definition-list description (`<dd>`). A leaf that accepts lazy
    /// continuation lines like a paragraph; `level` is 1 when loose (a blank line
    /// preceded the `:` marker, so the body is wrapped in `<p>`), 0 when tight.
    DefDesc,
    /// Leaf directive `::name[label]{attrs}` (one line). Carries its payload via
    /// `Node::fn_idx` → [`Tree::directives`]; the `[label]` is inline content.
    ///
    /// The variant (and its `ContainerDirective` sibling) stays compiled even
    /// without the `directives` feature (only their *producer* `start_directive`
    /// and the heavy directive code are cfg'd out, so no such node is ever
    /// created): removing a variant shrinks the hot `continue_block`/`can_contain`
    /// matches and perturbs their codegen (measured +2% on the default path).
    LeafDirective,
    /// Container directive `:::name[label]{attrs}` … `:::` — a block container
    /// (its content parses as markdown). Colon-fenced like a code block
    /// (`fence_char`/`fence_len`); payload via `Node::fn_idx`.
    ContainerDirective,
    /// SPIKE (`ast` feature): a link reference definition `[label]: url "title"`.
    /// Renders to nothing (HTML output unchanged); carries an index into
    /// [`Tree::defs`] via `Node::def` for the mdast `definition` node.
    #[cfg(feature = "ast")]
    Definition,
    /// GFM pipe table (opt-in). Content holds the raw rows (header, delimiter,
    /// then data rows), each `\n`-terminated; cells are parsed at render time.
    #[cfg(feature = "gfm")]
    Table,
}

/// SPIKE (`ast` feature): payload for a `definition` mdast node — all fields
/// already decoded/normalized to match `mdast-util-from-markdown`.
#[cfg(feature = "ast")]
#[derive(Clone)]
pub struct DefData {
    pub label: String,
    pub identifier: String,
    pub url: String,
    pub title: Option<String>,
}

/// Payload for a GFM `footnoteDefinition` — the raw label and its lowercased
/// identifier (the matching key, mirroring mdast's `label`/`identifier`).
#[cfg(feature = "footnotes")]
#[derive(Clone)]
pub struct FnDef {
    pub label: String,
    pub identifier: String,
}

/// Payload for a block directive (`leafDirective` / `containerDirective`): its
/// `name`, ordered `attributes`, and the source byte range of its `[label]`
/// content (if any). Indexed by `Node::fn_idx` (reused — a node is never both a
/// footnote definition and a directive) into [`Tree::directives`].
#[cfg(feature = "directives")]
#[derive(Clone)]
pub struct DirData {
    pub name: String,
    pub attrs: Vec<(String, String)>,
    pub label: Option<(u32, u32)>,
}

#[derive(Clone)]
pub struct ListData {
    pub ordered: bool,
    pub bullet: u8,
    pub start: u64,
    pub delimiter: u8,
    pub padding: usize,
    pub marker_offset: usize,
    pub tight: bool,
    /// SPIKE (`ast` feature): mdast `list.spread` — a blank line occurs *between
    /// items* (distinct from CommonMark's combined `tight`, which also folds in
    /// blanks *within* an item; see [`Node::item_spread`]).
    #[cfg(feature = "ast")]
    pub spread: bool,
}

pub struct Node {
    pub kind: Kind,
    /// Children as an intrusive first-child/next-sibling list (indices into the
    /// nodes arena, `NO_NODE` for none) — no per-node `Vec` allocation, and the
    /// walk stays inside the arena for cache locality.
    first_child: u32,
    last_child: u32,
    next_sibling: u32,
    pub parent: usize,
    open: bool,
    last_line_blank: bool,
    start_line: u32,
    /// Raw text as a `[cstart, cend)` range. `content_src` selects the backing
    /// store: the original source (borrowed, no copy) or the tree's `buf`
    /// (assembled — used when container prefixes/indent were stripped).
    cstart: u32,
    cend: u32,
    content_src: bool,
    pub level: u8,
    pub fenced: bool,
    fence_char: u8,
    fence_len: usize,
    fence_offset: usize,
    /// Fenced-code info string as a `[start, end)` range in the same store as
    /// the content (selected by `content_src`) — no per-block allocation.
    info_start: u32,
    info_end: u32,
    html_kind: u8,
    /// GFM footnote definition: index into [`Tree::fn_defs`] (`u32::MAX` = not a
    /// footnote definition). Always compiled; only set for `Kind::FootnoteDef`.
    fn_idx: u32,
    pub list: Option<ListData>,
    /// SPIKE (`ast` feature): index into [`Tree::defs`] for `Kind::Definition`
    /// nodes (meaningless otherwise).
    #[cfg(feature = "ast")]
    pub def: u32,
    /// SPIKE (`ast` feature): mdast `listItem.spread` for `Kind::Item` nodes — a
    /// blank line occurs between this item's own block children.
    #[cfg(feature = "ast")]
    pub item_spread: bool,
    /// SPIKE (`ast` feature): end offset of an `Kind::HtmlBlock`'s mdast `value`
    /// (which differs from the HTML-render `cend`: mdast keeps the trailing
    /// newline for a type-1 block ended by EOF, and drops exactly one otherwise).
    #[cfg(feature = "ast")]
    html_ast_cend: u32,
    /// SPIKE (`ast` feature): set when a `Kind::HtmlBlock` closed via its explicit
    /// end condition (types 1–5) rather than EOF / a blank line.
    #[cfg(feature = "ast")]
    html_closed_by_cond: bool,
    /// SPIKE (`ast` feature): the node's raw source span `[start, end)`, tracked
    /// even after the content is materialized/de-indented into `buf`. Used to
    /// recover reference-definition labels with their original indentation
    /// (mdast keeps it; the de-indented buffer does not). `u32::MAX` start = unset.
    #[cfg(feature = "ast")]
    src_start: u32,
    #[cfg(feature = "ast")]
    src_end: u32,
}

impl Node {
    fn new(kind: Kind, parent: usize, line: u32) -> Self {
        Node {
            kind,
            first_child: NO_NODE,
            last_child: NO_NODE,
            next_sibling: NO_NODE,
            parent,
            open: true,
            last_line_blank: false,
            start_line: line,
            cstart: 0,
            cend: 0,
            content_src: false,
            level: 0,
            fenced: false,
            fence_char: 0,
            fence_len: 0,
            fence_offset: 0,
            info_start: 0,
            info_end: 0,
            html_kind: 0,
            fn_idx: u32::MAX,
            list: None,
            #[cfg(feature = "ast")]
            def: 0,
            #[cfg(feature = "ast")]
            item_spread: false,
            #[cfg(feature = "ast")]
            html_ast_cend: 0,
            #[cfg(feature = "ast")]
            html_closed_by_cond: false,
            #[cfg(feature = "ast")]
            src_start: u32::MAX,
            #[cfg(feature = "ast")]
            src_end: 0,
        }
    }
}

pub struct Tree<'a> {
    pub nodes: Vec<Node>,
    pub root: usize,
    pub refmap: RefMap,
    pub source_len: usize,
    pub opts: Options,
    /// The original input; nodes with `content_src` index into it (borrowed).
    source: &'a str,
    /// Buffer for assembled text (block quotes, lists, code/HTML literals).
    buf: String,
    /// GFM footnote-definition payloads, indexed by `Node::fn_idx`.
    #[cfg(feature = "footnotes")]
    pub fn_defs: Vec<FnDef>,
    /// GFM footnote labels (lowercased) that have ≥1 definition — the set a
    /// `[^label]` reference must hit to be a `footnoteReference`.
    #[cfg(feature = "footnotes")]
    pub footnote_ids: std::collections::HashSet<String>,
    /// Block-directive payloads (`Kind::LeafDirective`/`ContainerDirective`),
    /// indexed by `Node::fn_idx`.
    #[cfg(feature = "directives")]
    pub directives: Vec<DirData>,
    /// SPIKE (`ast` feature): payloads for `Kind::Definition` nodes; indexed by
    /// `Node::def`.
    #[cfg(feature = "ast")]
    pub defs: Vec<DefData>,
    /// SPIKE (`ast` feature): piecewise `buf`→source map (see the parser field).
    #[cfg(feature = "ast")]
    pub buf_segs: Vec<(u32, u32)>,
}

impl Tree<'_> {
    /// The raw text of node `idx` (inline source or code/HTML literal).
    pub fn content(&self, idx: usize) -> &str {
        let n = &self.nodes[idx];
        let store = if n.content_src {
            self.source
        } else {
            &self.buf
        };
        &store[n.cstart as usize..n.cend as usize]
    }

    /// GFM footnote-definition payload (label + identifier) for a
    /// `Kind::FootnoteDef` node.
    #[cfg(feature = "footnotes")]
    pub fn fn_def(&self, idx: usize) -> &FnDef {
        &self.fn_defs[self.nodes[idx].fn_idx as usize]
    }

    /// Block-directive payload for a `Kind::LeafDirective`/`ContainerDirective`.
    #[cfg(feature = "directives")]
    pub fn directive(&self, idx: usize) -> &DirData {
        &self.directives[self.nodes[idx].fn_idx as usize]
    }

    /// A raw slice of the original source by byte range (e.g. a directive label).
    #[cfg(feature = "directives")]
    pub fn source_range(&self, start: u32, end: u32) -> &str {
        &self.source[start as usize..end as usize]
    }

    /// Consume the tree, returning its buffers for reuse by `parse_with`.
    pub fn recycle(self) -> (Vec<Node>, String, RefMap) {
        (self.nodes, self.buf, self.refmap)
    }

    /// First child of node `idx` in the intrusive child list, if any.
    pub fn first_child(&self, idx: usize) -> Option<usize> {
        let c = self.nodes[idx].first_child;
        (c != NO_NODE).then_some(c as usize)
    }

    /// Next sibling of node `idx`, if any.
    pub fn next_sibling(&self, idx: usize) -> Option<usize> {
        let s = self.nodes[idx].next_sibling;
        (s != NO_NODE).then_some(s as usize)
    }

    /// The fenced-code info string of node `idx` (raw; unescape at render).
    pub fn info(&self, idx: usize) -> &str {
        let n = &self.nodes[idx];
        let store = if n.content_src {
            self.source
        } else {
            &self.buf
        };
        &store[n.info_start as usize..n.info_end as usize]
    }

    /// SPIKE: the 1-based source line on which node `idx` opened (0 for the
    /// document root). Used to attach unist `position` to block nodes.
    pub fn start_line(&self, idx: usize) -> u32 {
        self.nodes[idx].start_line
    }

    /// SPIKE (`ast` feature): the `definition` payload of a `Kind::Definition`
    /// node.
    #[cfg(feature = "ast")]
    pub fn definition(&self, idx: usize) -> &DefData {
        &self.defs[self.nodes[idx].def as usize]
    }

    /// SPIKE (`ast` feature): map a content byte offset (relative to node `idx`'s
    /// content) to a source byte offset. For source-borrowed content this is just
    /// `cstart + off`; for buffered content (blockquote/list) it walks the
    /// `buf`→source segment map.
    #[cfg(feature = "ast")]
    pub fn content_to_src(&self, idx: usize, off: u32) -> u32 {
        let n = &self.nodes[idx];
        let buf_off = n.cstart + off;
        if n.content_src {
            return buf_off;
        }
        // Largest segment whose buf_off <= buf_off; source advances 1:1 within it.
        let i = self.buf_segs.partition_point(|&(b, _)| b <= buf_off);
        if i == 0 {
            return buf_off; // before any segment (shouldn't happen)
        }
        let (b, s) = self.buf_segs[i - 1];
        s + (buf_off - b)
    }

    /// SPIKE (`ast` feature): a node's raw source byte span `(start, end)`.
    /// `start == u32::MAX` means unset (e.g. a synthesized container before its
    /// children resolve it). `end` may include a trailing newline (callers trim).
    #[cfg(feature = "ast")]
    pub fn src_span(&self, idx: usize) -> (u32, u32) {
        (self.nodes[idx].src_start, self.nodes[idx].src_end)
    }

    /// SPIKE (`ast` feature): the mdast `value` of a `Kind::HtmlBlock` (keeps the
    /// trailing newline per mdast's rule, unlike the HTML-render [`Self::content`]).
    #[cfg(feature = "ast")]
    pub fn html_value(&self, idx: usize) -> &str {
        let n = &self.nodes[idx];
        let store = if n.content_src {
            self.source
        } else {
            &self.buf
        };
        &store[n.cstart as usize..n.html_ast_cend as usize]
    }

    /// SPIKE (`ast` feature): the mdast `position.end` source offset of a
    /// `Kind::HtmlBlock` — where its `value` ends, mapped from content space to
    /// source (buffered when inside a blockquote/list). The block's `src_end`
    /// may over-include trailing blank lines, so prefer this.
    #[cfg(feature = "ast")]
    pub fn html_ast_end(&self, idx: usize) -> u32 {
        let n = &self.nodes[idx];
        self.content_to_src(idx, n.html_ast_cend - n.cstart)
    }
}

/// Parse `src` (CommonMark, no options) into a block tree plus its link
/// reference definitions.
pub fn parse(src: &str) -> Tree<'_> {
    parse_with_opts(src, Options::default())
}

/// Parse `src` with opt-in [`Options`].
pub fn parse_with_opts(src: &str, opts: Options) -> Tree<'_> {
    Parser::with(Vec::new(), String::new(), RefMap::new(), opts).parse(src)
}

/// Like `parse_with_opts`, but reuses the given (recycled) buffers instead of
/// allocating fresh ones. Pair with [`Tree::recycle`] for repeated parsing.
pub fn parse_with(
    src: &str,
    opts: Options,
    nodes: Vec<Node>,
    buf: String,
    refmap: RefMap,
) -> Tree<'_> {
    Parser::with(nodes, buf, refmap, opts).parse(src)
}

fn peek(line: &[u8], i: usize) -> Option<u8> {
    line.get(i).copied()
}

fn is_space_or_tab(c: Option<u8>) -> bool {
    matches!(c, Some(b' ') | Some(b'\t'))
}

struct Parser<'a> {
    nodes: Vec<Node>,
    tip: usize,
    oldtip: usize,
    last_matched_container: usize,
    all_closed: bool,
    refmap: RefMap,
    /// The full input (for borrowing contiguous block content).
    source: &'a str,
    /// Buffer for assembled (non-contiguous) text.
    buf: String,
    // line state — borrows the source line (no per-line allocation)
    line: &'a [u8],
    /// Source byte offset where the current line begins.
    line_src_start: usize,
    line_number: u32,
    offset: usize,
    column: usize,
    next_nonspace: usize,
    next_nonspace_column: usize,
    indent: usize,
    indented: bool,
    blank: bool,
    /// Whether the immediately-preceding line was blank. Used by the definition
    /// list grammar to mark a `: definition` loose (its body wrapped in `<p>`)
    /// when a blank line separates it from the term.
    prev_blank: bool,
    partially_consumed_tab: bool,
    opts: Options,
    /// GFM footnote-definition payloads, in creation order; a node's
    /// `Node::fn_idx` indexes here.
    #[cfg(feature = "footnotes")]
    fn_defs: Vec<FnDef>,
    /// GFM footnote labels (lowercased) seen as definitions — the reference set.
    #[cfg(feature = "footnotes")]
    footnote_ids: std::collections::HashSet<String>,
    /// Block-directive payloads, in creation order; a node's `Node::fn_idx`
    /// indexes here.
    #[cfg(feature = "directives")]
    directives: Vec<DirData>,
    /// SPIKE (`ast` feature): payloads for `Kind::Definition` nodes, in creation
    /// order; a node's `Node::def` indexes here.
    #[cfg(feature = "ast")]
    defs: Vec<DefData>,
    /// SPIKE (`ast` feature): piecewise `buf`→source map for buffered content
    /// (blockquote/list, where prefixes are stripped). Each `(buf_off, src_off)`
    /// means buf bytes from `buf_off` onward map 1:1 to source from `src_off`
    /// (until the next entry). Lets inline nodes in buffered blocks recover exact
    /// source positions.
    #[cfg(feature = "ast")]
    buf_segs: Vec<(u32, u32)>,
    /// SPIKE (`ast` feature): set during the end-of-document finalize sweep. An
    /// unclosed fenced code block keeps its trailing newline only when it ends at
    /// EOF; one ended mid-document (blank line / container exit) drops it.
    #[cfg(feature = "ast")]
    at_eof: bool,
}

impl<'a> Parser<'a> {
    /// Build a parser from recycled buffers (cleared and reused), so repeated
    /// parsing avoids re-allocating the node arena, text buffer, and ref map.
    fn with(mut nodes: Vec<Node>, mut buf: String, mut refmap: RefMap, opts: Options) -> Self {
        nodes.clear();
        buf.clear();
        refmap.clear();
        nodes.push(Node::new(Kind::Document, 0, 0));
        Parser {
            nodes,
            tip: 0,
            oldtip: 0,
            last_matched_container: 0,
            all_closed: true,
            refmap,
            source: "",
            buf,
            line: &[],
            line_src_start: 0,
            line_number: 0,
            offset: 0,
            column: 0,
            next_nonspace: 0,
            next_nonspace_column: 0,
            indent: 0,
            indented: false,
            blank: false,
            prev_blank: false,
            partially_consumed_tab: false,
            opts,
            #[cfg(feature = "footnotes")]
            fn_defs: Vec::new(),
            #[cfg(feature = "footnotes")]
            footnote_ids: std::collections::HashSet::new(),
            #[cfg(feature = "directives")]
            directives: Vec::new(),
            #[cfg(feature = "ast")]
            defs: Vec::new(),
            #[cfg(feature = "ast")]
            buf_segs: Vec::new(),
            #[cfg(feature = "ast")]
            at_eof: false,
        }
    }

    fn last_child(&self, n: usize) -> Option<usize> {
        let lc = self.nodes[n].last_child;
        (lc != NO_NODE).then_some(lc as usize)
    }

    // ---- line position helpers ------------------------------------------

    fn find_next_nonspace(&mut self) {
        let mut i = self.offset;
        let mut cols = self.column;
        loop {
            match peek(self.line, i) {
                Some(b' ') => {
                    i += 1;
                    cols += 1;
                }
                Some(b'\t') => {
                    i += 1;
                    cols += 4 - (cols % 4);
                }
                _ => break,
            }
        }
        self.blank = matches!(peek(self.line, i), None | Some(b'\n'));
        self.next_nonspace = i;
        self.next_nonspace_column = cols;
        self.indent = cols - self.column;
        self.indented = self.indent >= CODE_INDENT;
    }

    fn advance_next_nonspace(&mut self) {
        self.offset = self.next_nonspace;
        self.column = self.next_nonspace_column;
        self.partially_consumed_tab = false;
    }

    fn advance_offset(&mut self, mut count: usize, columns: bool) {
        while count > 0 {
            match peek(self.line, self.offset) {
                Some(b'\t') => {
                    let chars_to_tab = 4 - (self.column % 4);
                    if columns {
                        self.partially_consumed_tab = chars_to_tab > count;
                        let consume = chars_to_tab.min(count);
                        self.column += consume;
                        if !self.partially_consumed_tab {
                            self.offset += 1;
                        }
                        count -= consume;
                    } else {
                        self.partially_consumed_tab = false;
                        self.column += chars_to_tab;
                        self.offset += 1;
                        count -= 1;
                    }
                }
                Some(_) => {
                    self.partially_consumed_tab = false;
                    self.offset += 1;
                    self.column += 1;
                    count -= 1;
                }
                None => break,
            }
        }
    }

    // ---- tree construction ----------------------------------------------

    fn add_child(&mut self, kind: Kind) -> usize {
        while !can_contain(self.nodes[self.tip].kind, kind) {
            self.finalize(self.tip);
        }
        let idx = self.nodes.len();
        let parent = self.tip;
        let mut node = Node::new(kind, parent, self.line_number);
        // Paragraphs try to borrow a contiguous source slice; other leaves
        // (ATX heading, code, HTML) assemble into `buf`. Set the buffered start.
        node.content_src = matches!(
            kind,
            Kind::Paragraph | Kind::CodeBlock | Kind::HtmlBlock | Kind::DefDesc
        );
        node.cstart = self.buf.len() as u32;
        node.cend = node.cstart;
        // SPIKE (`ast`): the block's mdast `position.start` is its first non-space
        // (marker for containers/atx/fences, first char for paragraphs).
        #[cfg(feature = "ast")]
        {
            node.src_start = (self.line_src_start + self.next_nonspace) as u32;
        }
        self.nodes.push(node);
        // Append to the parent's intrusive child list.
        let last = self.nodes[parent].last_child;
        if last == NO_NODE {
            self.nodes[parent].first_child = idx as u32;
        } else {
            self.nodes[last as usize].next_sibling = idx as u32;
        }
        self.nodes[parent].last_child = idx as u32;
        self.tip = idx;
        idx
    }

    /// SPIKE (`ast` feature): create a new leaf node and splice it into the child
    /// list immediately *before* `sibling` (same parent), preserving source
    /// order. Used to emit `Kind::Definition` nodes ahead of the paragraph they
    /// were stripped from. Does not touch `tip`.
    #[cfg(feature = "ast")]
    fn insert_before(&mut self, sibling: usize, kind: Kind) -> usize {
        let parent = self.nodes[sibling].parent;
        let idx = self.nodes.len();
        self.nodes.push(Node::new(kind, parent, self.line_number));
        let sib32 = sibling as u32;
        if self.nodes[parent].first_child == sib32 {
            self.nodes[parent].first_child = idx as u32;
        } else {
            let mut prev = self.nodes[parent].first_child;
            while self.nodes[prev as usize].next_sibling != sib32 {
                prev = self.nodes[prev as usize].next_sibling;
            }
            self.nodes[prev as usize].next_sibling = idx as u32;
        }
        self.nodes[idx].next_sibling = sib32;
        idx
    }

    /// SPIKE (`ast` feature): register the leading reference definitions of a
    /// paragraph/heading — insert each into the [`RefMap`] (first wins) and, as a
    /// side effect, emit a `Kind::Definition` node before `before` for each.
    /// SPIKE (`ast` feature): replace each def's label (de-indented by the buffer)
    /// with its raw source form, so mdast's `label` keeps the original
    /// continuation-line indentation. Only safe for **top-level** paragraphs,
    /// whose source span is contiguous (no stripped container prefixes); nested
    /// defs keep the buffer label (correct `identifier`, rare label-whitespace
    /// divergence). No-op unless the source re-parse yields the same def count.
    #[cfg(feature = "ast")]
    fn recover_raw_labels(&self, node: usize, defs: &mut [(String, String, Option<String>)]) {
        if self.nodes[node].parent != 0 {
            return;
        }
        let (ss, se) = (self.nodes[node].src_start, self.nodes[node].src_end);
        if ss == u32::MAX || ss >= se || (se as usize) > self.source.len() {
            return;
        }
        let src_defs = take_ref_defs(&self.source[ss as usize..se as usize]).1;
        if src_defs.len() == defs.len() {
            for (d, sd) in defs.iter_mut().zip(src_defs) {
                d.0 = sd.0;
            }
        }
    }

    #[cfg(feature = "ast")]
    fn emit_defs(&mut self, before: usize, defs: Vec<(String, String, Option<String>)>) {
        // For a top-level paragraph (contiguous in source), recover each def's
        // exact source span; otherwise approximate with the paragraph's span.
        let (ss, se) = (self.nodes[before].src_start, self.nodes[before].src_end);
        let spans = if self.nodes[before].parent == 0
            && ss != u32::MAX
            && ss < se
            && (se as usize) <= self.source.len()
        {
            let s = crate::inline::take_ref_def_spans(&self.source[ss as usize..se as usize]);
            (s.len() == defs.len()).then_some(s)
        } else {
            None
        };
        for (i, (label, dest, title)) in defs.into_iter().enumerate() {
            let identifier = crate::inline::normalize_label(&label).into_owned();
            let di = self.defs.len() as u32;
            self.defs.push(DefData {
                label: crate::inline::unescape_string(&label).into_owned(),
                identifier: identifier.clone(),
                url: crate::inline::unescape_string(&dest).into_owned(),
                title: title
                    .as_deref()
                    .map(|t| crate::inline::unescape_string(t).into_owned()),
            });
            let dn = self.insert_before(before, Kind::Definition);
            self.nodes[dn].def = di;
            match &spans {
                Some(sp) => {
                    self.nodes[dn].src_start = ss + sp[i].0 as u32;
                    self.nodes[dn].src_end = ss + sp[i].1 as u32;
                }
                None => {
                    self.nodes[dn].src_start = self.nodes[before].src_start;
                    self.nodes[dn].src_end = self.nodes[before].src_end;
                }
            }
            self.refmap.entry(identifier).or_insert((dest, title));
        }
    }

    fn add_line(&mut self) {
        let tip = self.tip;
        // SPIKE (`ast`): remember the raw source span (survives materialization),
        // for recovering ref-def labels with their original indentation.
        #[cfg(feature = "ast")]
        {
            let line_end = self.line_src_start + self.line.len();
            let nl = (line_end < self.source.len() && self.source.as_bytes()[line_end] == b'\n')
                as usize;
            if self.nodes[tip].src_start == u32::MAX {
                self.nodes[tip].src_start = (self.line_src_start + self.offset) as u32;
            }
            self.nodes[tip].src_end = (line_end + nl) as u32;
        }
        // Try to (keep) borrowing a contiguous slice of the source. Borrowed
        // ranges include each line's trailing newline (so code/HTML literals,
        // which need it, work); the contiguous next line begins exactly at the
        // current end.
        if self.nodes[tip].content_src {
            let cend = self.nodes[tip].cend as usize;
            let first = self.nodes[tip].cstart as usize == cend;
            let contiguous = self.offset == 0 && self.line_src_start == cend;
            let line_end = self.line_src_start + self.line.len();
            let has_nl = line_end < self.source.len() && self.source.as_bytes()[line_end] == b'\n';
            // Code/HTML literals require a trailing newline; a final line at EOF
            // without one must be assembled instead.
            let needs_nl = matches!(self.nodes[tip].kind, Kind::CodeBlock | Kind::HtmlBlock);
            if !self.partially_consumed_tab && (first || contiguous) && (has_nl || !needs_nl) {
                if first {
                    self.nodes[tip].cstart = (self.line_src_start + self.offset) as u32;
                }
                self.nodes[tip].cend = (line_end + has_nl as usize) as u32;
                return;
            }
            // Contiguity broken: copy the borrowed prefix into `buf`, continue there.
            self.materialize(tip);
        }
        // Buffered append.
        if self.partially_consumed_tab {
            self.offset += 1;
            let chars_to_tab = 4 - (self.column % 4);
            for _ in 0..chars_to_tab {
                self.buf.push(' ');
            }
        }
        // SPIKE (`ast`): `rest` (after prefix stripping) maps 1:1 to source from
        // `line_src_start + offset` — record the breakpoint so inline nodes in
        // buffered blocks recover source positions.
        #[cfg(feature = "ast")]
        self.buf_segs.push((
            self.buf.len() as u32,
            (self.line_src_start + self.offset) as u32,
        ));
        let rest = &self.line[self.offset..];
        // line never contains an embedded NUL; push as UTF-8.
        self.buf.push_str(std::str::from_utf8(rest).unwrap_or(""));
        self.buf.push('\n');
        self.nodes[tip].cend = self.buf.len() as u32;
    }

    /// Move a node's borrowed source range into `buf` so further lines append.
    /// The borrowed range already ends with a newline (contiguity only breaks
    /// after a `\n`-terminated line), so none is added.
    fn materialize(&mut self, tip: usize) {
        let (s, e) = (
            self.nodes[tip].cstart as usize,
            self.nodes[tip].cend as usize,
        );
        let start = self.buf.len();
        if s != e {
            // SPIKE (`ast`): the copied prefix maps 1:1 to source `[s, e)`.
            #[cfg(feature = "ast")]
            self.buf_segs.push((start as u32, s as u32));
            self.buf.push_str(&self.source[s..e]);
        }
        self.nodes[tip].content_src = false;
        self.nodes[tip].cstart = start as u32;
        self.nodes[tip].cend = self.buf.len() as u32;
    }

    /// SPIKE (`ast`): map a `buf` byte offset to source via the segment map.
    #[cfg(feature = "ast")]
    fn map_buf_off(&self, buf_off: u32) -> u32 {
        let i = self.buf_segs.partition_point(|&(b, _)| b <= buf_off);
        if i == 0 {
            return buf_off;
        }
        let (b, s) = self.buf_segs[i - 1];
        s + (buf_off - b)
    }

    fn close_unmatched_blocks(&mut self) {
        if !self.all_closed {
            while self.oldtip != self.last_matched_container {
                let parent = self.nodes[self.oldtip].parent;
                self.finalize(self.oldtip);
                self.oldtip = parent;
            }
            self.all_closed = true;
        }
    }

    // ---- finalize -------------------------------------------------------

    /// `(cstart, cend, content_src)` for node `idx`.
    fn content_range(&self, idx: usize) -> (usize, usize, bool) {
        let n = &self.nodes[idx];
        (n.cstart as usize, n.cend as usize, n.content_src)
    }

    fn finalize(&mut self, idx: usize) {
        let parent = self.nodes[idx].parent;
        self.nodes[idx].open = false;

        match self.nodes[idx].kind {
            Kind::Paragraph => {
                let (s, e, csrc) = self.content_range(idx);
                // All buffer reads in one scope so the store borrow ends before
                // `refmap` is mutated; `defs` is owned, the rest are offsets.
                let (lead, off, defs, empty, hl, inner_len) = {
                    let store: &str = if csrc { self.source } else { &self.buf };
                    let sl = &store[s..e];
                    let lead = sl.len() - sl.trim_start_matches(['\n', ' ', '\t']).len();
                    let (off, defs) = take_ref_defs(&store[s + lead..e]);
                    let body = &store[s + lead + off..e];
                    let hl = body.len() - body.trim_start_matches('\n').len();
                    let inner = body.trim_matches('\n');
                    let empty = body.trim_matches(['\n', ' ', '\t']).is_empty();
                    (lead, off, defs, empty, hl, inner.len())
                };
                #[cfg(feature = "ast")]
                {
                    let mut defs = defs;
                    self.recover_raw_labels(idx, &mut defs);
                    self.emit_defs(idx, defs);
                }
                #[cfg(not(feature = "ast"))]
                for (label, dest, title) in defs {
                    self.refmap
                        .entry(crate::inline::normalize_label(&label).into_owned())
                        .or_insert((dest, title));
                }
                let bs = s + lead + off;
                if empty {
                    self.unlink(idx); // pure reference definitions
                } else {
                    self.nodes[idx].cstart = (bs + hl) as u32;
                    self.nodes[idx].cend = (bs + hl + inner_len) as u32;
                    // SPIKE (`ast`): leading defs shift the paragraph's start past
                    // the (now-stripped) definition lines.
                    #[cfg(feature = "ast")]
                    {
                        let cs = (bs + hl) as u32;
                        self.nodes[idx].src_start = if csrc { cs } else { self.map_buf_off(cs) };
                    }
                }
            }
            Kind::Heading => {
                let (s, e, csrc) = self.content_range(idx);
                let (hl, tlen) = {
                    let store: &str = if csrc { self.source } else { &self.buf };
                    let sl = &store[s..e];
                    let hl = sl.len() - sl.trim_start_matches(['\n', ' ', '\t']).len();
                    (hl, sl.trim_matches(['\n', ' ', '\t']).len())
                };
                self.nodes[idx].cstart = (s + hl) as u32;
                self.nodes[idx].cend = (s + hl + tlen) as u32;
            }
            Kind::CodeBlock => {
                let (s, e, csrc) = self.content_range(idx);
                if self.nodes[idx].fenced {
                    // First line is the info string (recorded as a range; the
                    // renderer takes its first word and unescapes lazily); the
                    // rest is the literal.
                    let (nl, ts, te) = {
                        let store: &str = if csrc { self.source } else { &self.buf };
                        let nl = store[s..e].find('\n').map_or(e - s, |p| p);
                        let first = &store[s..s + nl];
                        let lead = first.len() - first.trim_start().len();
                        let trimmed = first.trim();
                        (nl, s + lead, s + lead + trimmed.len())
                    };
                    self.nodes[idx].info_start = ts as u32;
                    self.nodes[idx].info_end = te as u32;
                    self.nodes[idx].cstart = (s + nl + 1).min(e) as u32;
                    // SPIKE (`ast`): an unclosed fenced block ended mid-document
                    // (blank line / container exit) drops its trailing newline;
                    // one ended at EOF (or by its closing fence) keeps its span.
                    #[cfg(feature = "ast")]
                    if !self.at_eof {
                        let se = self.nodes[idx].src_end as usize;
                        let b = self.source.as_bytes();
                        if se > 0 && b[se - 1] == b'\n' {
                            let mut x = se - 1;
                            if x > 0 && b[x - 1] == b'\r' {
                                x -= 1;
                            }
                            self.nodes[idx].src_end = x as u32;
                        }
                    }
                } else {
                    let keep = {
                        let store: &str = if csrc { self.source } else { &self.buf };
                        code_indented_end(&store[s..e])
                    };
                    self.nodes[idx].cend = (s + keep) as u32;
                }
            }
            Kind::HtmlBlock => {
                let (s, e, csrc) = self.content_range(idx);
                let keep = {
                    let store: &str = if csrc { self.source } else { &self.buf };
                    html_trim_end(&store[s..e])
                };
                // mdast `value` differs from the trimmed HTML-render content: a
                // type-1 block (`<script>`/`<style>`/`<pre>`) ended by EOF keeps
                // its raw span verbatim; every other block drops exactly the
                // final line ending.
                #[cfg(feature = "ast")]
                {
                    let store: &str = if csrc { self.source } else { &self.buf };
                    let bytes = store.as_bytes();
                    let ast_end =
                        if self.nodes[idx].html_kind == 1 && !self.nodes[idx].html_closed_by_cond {
                            e
                        } else {
                            let mut x = e;
                            if x > s && bytes[x - 1] == b'\n' {
                                x -= 1;
                                if x > s && bytes[x - 1] == b'\r' {
                                    x -= 1;
                                }
                            }
                            x
                        };
                    self.nodes[idx].html_ast_cend = ast_end as u32;
                }
                self.nodes[idx].cend = (s + keep) as u32;
            }
            Kind::List => {
                let tight = self.compute_tight(idx);
                if let Some(ld) = &mut self.nodes[idx].list {
                    ld.tight = tight;
                }
                #[cfg(feature = "ast")]
                self.compute_spread(idx);
            }
            // Arm stays compiled (keeps the hot finalize match stable); without
            // the `deflist` feature no `DefList` node is ever created, so the body
            // is gated out and the arm folds into the `_ => {}` catch-all.
            Kind::DefList => {
                // A trailing plain paragraph is a dangling term candidate (no
                // description ever followed it); evict it to become the list's
                // next sibling so it renders as an ordinary paragraph.
                #[cfg(feature = "deflist")]
                {
                    let lc = self.nodes[idx].last_child;
                    if lc != NO_NODE && self.nodes[lc as usize].kind == Kind::Paragraph {
                        self.unlink(lc as usize);
                        let after = self.nodes[idx].next_sibling;
                        self.nodes[lc as usize].parent = parent;
                        self.nodes[lc as usize].next_sibling = after;
                        self.nodes[idx].next_sibling = lc;
                        if self.nodes[parent].last_child == idx as u32 {
                            self.nodes[parent].last_child = lc;
                        }
                    }
                }
            }
            _ => {}
        }
        let _ = parent;
        if self.tip == idx {
            self.tip = self.nodes[idx].parent;
        }
    }

    fn unlink(&mut self, idx: usize) {
        let parent = self.nodes[idx].parent;
        let idx32 = idx as u32;
        let next = self.nodes[idx].next_sibling;
        if self.nodes[parent].first_child == idx32 {
            self.nodes[parent].first_child = next;
            if self.nodes[parent].last_child == idx32 {
                self.nodes[parent].last_child = NO_NODE;
            }
        } else {
            let mut prev = self.nodes[parent].first_child;
            while self.nodes[prev as usize].next_sibling != idx32 {
                prev = self.nodes[prev as usize].next_sibling;
            }
            self.nodes[prev as usize].next_sibling = next;
            if self.nodes[parent].last_child == idx32 {
                self.nodes[parent].last_child = prev;
            }
        }
    }

    fn compute_tight(&self, list: usize) -> bool {
        let mut item = self.nodes[list].first_child;
        while item != NO_NODE {
            let item_last = self.nodes[item as usize].next_sibling == NO_NODE;
            // blank line at end of an item that is not the last → loose
            if !item_last && self.ends_with_blank_line(item as usize) {
                return false;
            }
            let mut sub = self.nodes[item as usize].first_child;
            while sub != NO_NODE {
                let sub_last = self.nodes[sub as usize].next_sibling == NO_NODE;
                if self.ends_with_blank_line(sub as usize) && !(item_last && sub_last) {
                    return false;
                }
                sub = self.nodes[sub as usize].next_sibling;
            }
            item = self.nodes[item as usize].next_sibling;
        }
        true
    }

    /// SPIKE (`ast` feature): split CommonMark's combined looseness into mdast's
    /// two spread bits — `list.spread` (blank *between* items) on the list, and
    /// `listItem.spread` (blank *between an item's own block children*) on each
    /// item. The disjunction `list.spread || any(item.spread)` equals
    /// `!compute_tight`, so HTML rendering (which uses `tight`) is unaffected.
    #[cfg(feature = "ast")]
    fn compute_spread(&mut self, list: usize) {
        let mut list_spread = false;
        let mut item = self.nodes[list].first_child;
        while item != NO_NODE {
            let iu = item as usize;
            let item_last = self.nodes[iu].next_sibling == NO_NODE;
            // A blank after a non-last item ⇒ the list is spread.
            if !item_last && self.ends_with_blank_line(iu) {
                list_spread = true;
            }
            // A blank between two of an item's own block children ⇒ item spread.
            let mut item_spread = false;
            let mut sub = self.nodes[iu].first_child;
            while sub != NO_NODE {
                let sub_last = self.nodes[sub as usize].next_sibling == NO_NODE;
                if !sub_last && self.ends_with_blank_line(sub as usize) {
                    item_spread = true;
                }
                sub = self.nodes[sub as usize].next_sibling;
            }
            self.nodes[iu].item_spread = item_spread;
            item = self.nodes[iu].next_sibling;
        }
        if let Some(ld) = &mut self.nodes[list].list {
            ld.spread = list_spread;
        }
    }

    fn ends_with_blank_line(&self, mut idx: usize) -> bool {
        loop {
            if self.nodes[idx].last_line_blank {
                return true;
            }
            if matches!(self.nodes[idx].kind, Kind::List | Kind::Item) {
                let last = self.nodes[idx].last_child;
                if last == NO_NODE {
                    return false;
                }
                idx = last as usize;
            } else {
                return false;
            }
        }
    }

    // ---- frontmatter (document-prefix grammar) --------------------------

    /// Detect and consume a leading YAML (`---`) / TOML (`+++`) frontmatter
    /// block. Returns the byte offset to resume the block loop at (just past the
    /// closing fence line), or `None` if the input does not open with a
    /// *complete* frontmatter block — in which case nothing is consumed and the
    /// fence text parses normally (an unmatched `---` becomes a thematic break).
    fn try_frontmatter(&mut self, bytes: &[u8]) -> Option<usize> {
        // The opening fence is the very first line, unindented, exactly three
        // `-`/`+` markers then only trailing spaces/tabs.
        let first_end = memchr1(bytes, b'\n').map_or(bytes.len(), |p| p);
        let first = trim_cr(&bytes[..first_end]);
        let marker = match first.first()? {
            b'-' => b'-',
            b'+' => b'+',
            _ => return None,
        };
        if !is_fm_fence(first, marker) {
            return None;
        }
        // A closing fence is required; without a newline after the opener there
        // can be no second line, so this is not frontmatter.
        if first_end >= bytes.len() {
            return None;
        }
        let content_start = first_end + 1;
        let mut pos = content_start;
        let mut lines = 1u32; // the opening fence line
        while pos < bytes.len() {
            let end = memchr1(&bytes[pos..], b'\n').map_or(bytes.len(), |p| pos + p);
            let line = trim_cr(&bytes[pos..end]);
            lines += 1;
            if is_fm_fence(line, marker) {
                // `cend` is the raw start of the closing line; `src_end` is the
                // end of the closing fence's content (incl. trailing spaces,
                // excl. the line ending) — mdast keeps a frontmatter's trailing
                // spaces but not its newline.
                self.push_frontmatter(marker, content_start, pos, pos + line.len());
                self.line_number = lines;
                return Some((end + 1).min(bytes.len()));
            }
            pos = end + 1;
        }
        None
    }

    /// Create the frontmatter node as the document's (already empty) first child.
    /// It is born closed — block phase 1 never descends into it.
    fn push_frontmatter(&mut self, marker: u8, cstart: usize, cend: usize, src_end: usize) {
        let idx = self.nodes.len();
        let mut node = Node::new(Kind::Frontmatter, 0, 1);
        node.open = false;
        node.content_src = true;
        node.cstart = cstart as u32;
        node.cend = cend as u32;
        node.level = if marker == b'+' { 1 } else { 0 }; // 0 = yaml, 1 = toml
        #[cfg(feature = "ast")]
        {
            node.src_start = 0;
            node.src_end = src_end as u32;
        }
        #[cfg(not(feature = "ast"))]
        let _ = src_end;
        self.nodes.push(node);
        self.nodes[0].first_child = idx as u32;
        self.nodes[0].last_child = idx as u32;
    }

    // ---- main loop ------------------------------------------------------

    fn parse(mut self, src: &'a str) -> Tree<'a> {
        self.source = src;
        // Rough upper bounds. `buf` only holds assembled (non-borrowed) text, so
        // it stays small for prose-heavy input.
        self.nodes.reserve(src.len() / 32);
        self.buf.reserve(src.len() / 4);
        // Iterate lines on the fly (SWAR `\n` search) rather than materializing
        // a Vec of every line — no big allocation, one pass, vectorized split.
        let bytes = src.as_bytes();
        let mut start = 0;
        // Frontmatter is a document-prefix grammar: a `---`/`+++` fence pair at
        // the very first byte. Consumed before the block loop so the rest parses
        // normally (and `---` only becomes a thematic break when there is no
        // matching close).
        if self.opts.frontmatter
            && let Some(resume) = self.try_frontmatter(bytes)
        {
            start = resume;
        }
        while start < bytes.len() {
            let end = memchr1(&bytes[start..], b'\n').map_or(bytes.len(), |p| start + p);
            self.line_src_start = start;
            self.incorporate_line(&bytes[start..end]);
            start = end + 1;
        }
        #[cfg(feature = "ast")]
        {
            self.at_eof = true;
        }
        while self.tip != 0 {
            let t = self.tip;
            self.finalize(t);
        }
        self.finalize(0);
        Tree {
            nodes: self.nodes,
            root: 0,
            refmap: self.refmap,
            source_len: src.len(),
            opts: self.opts,
            source: src,
            buf: self.buf,
            #[cfg(feature = "footnotes")]
            fn_defs: self.fn_defs,
            #[cfg(feature = "footnotes")]
            footnote_ids: self.footnote_ids,
            #[cfg(feature = "directives")]
            directives: self.directives,
            #[cfg(feature = "ast")]
            defs: self.defs,
            #[cfg(feature = "ast")]
            buf_segs: self.buf_segs,
        }
    }

    #[inline(always)]
    fn incorporate_line(&mut self, line: &'a [u8]) {
        let mut container = 0;
        self.oldtip = self.tip;
        self.line = line;
        self.line_number += 1;
        self.offset = 0;
        self.column = 0;
        self.partially_consumed_tab = false;
        self.blank = false;

        // Phase 1: descend through open containers, checking continuation.
        let mut all_matched = true;
        while let Some(lc) = self.last_child(container) {
            if !self.nodes[lc].open {
                break;
            }
            container = lc;
            self.find_next_nonspace();
            match self.continue_block(container) {
                0 => {}
                1 => {
                    all_matched = false;
                }
                2 => {
                    self.prev_blank = self.blank; // line fully consumed (code block)
                    return;
                }
                _ => unreachable!(),
            }
            if !all_matched {
                container = self.nodes[container].parent;
                break;
            }
        }

        self.all_closed = container == self.oldtip;
        self.last_matched_container = container;

        let mut matched_leaf = self.nodes[container].kind != Kind::Paragraph
            && accepts_lines(self.nodes[container].kind);

        // Phase 2: look for new block starts.
        while !matched_leaf {
            self.find_next_nonspace();
            // Fast skip: a non-indented line whose first non-space char can't
            // begin any block is plain paragraph text — bypass all matchers.
            let first = peek(self.line, self.next_nonspace);
            let fast_skip = !self.indented && !maybe_special(first);
            // A GFM table delimiter row can start with `|`/`:` (not in
            // `maybe_special`); only such lines need to reach `start_table`, so
            // the fast skip stays on for everything else. Compiled out off `gfm`.
            #[cfg(feature = "gfm")]
            let fast_skip =
                fast_skip && !(self.opts.tables && matches!(first, Some(b'|') | Some(b':')));
            // A footnote definition starts with `[` (not in `maybe_special`); keep
            // the fast skip on for `[` lines unless footnotes are enabled.
            #[cfg(feature = "footnotes")]
            let fast_skip = fast_skip && !(self.opts.footnotes && first == Some(b'['));
            // Definition-list markers and directives both start with `:` (not in
            // `maybe_special`); let `:` lines through to their matchers when
            // either extension is enabled.
            let fast_skip = fast_skip
                && !(((Options::DEFLIST && self.opts.deflist)
                    || (Options::DIRECTIVES && self.opts.directives))
                    && first == Some(b':'));
            if fast_skip {
                self.advance_next_nonspace();
                break;
            }
            let mut found = false;
            for start in 0..NUM_STARTS {
                match self.try_start(start, container) {
                    1 => {
                        container = self.tip;
                        found = true;
                        break;
                    }
                    2 => {
                        container = self.tip;
                        matched_leaf = true;
                        found = true;
                        break;
                    }
                    _ => {}
                }
            }
            if !found {
                self.advance_next_nonspace();
                break;
            }
        }

        // Phase 3: add text to the appropriate container.
        if !self.all_closed && !self.blank && self.nodes[self.tip].kind == Kind::Paragraph {
            self.add_line(); // lazy paragraph continuation
        } else {
            self.close_unmatched_blocks();
            if self.blank
                && let Some(lc) = self.last_child(container)
            {
                self.nodes[lc].last_line_blank = true;
            }
            // SPIKE (`ast`): a blank line carrying blockquote markers (e.g. ">>")
            // is absorbed by an open ancestor list — mdast extends the list's
            // position through it. (A bare blank line at the top level is not.)
            #[cfg(feature = "ast")]
            if self.blank {
                let mut list = usize::MAX;
                let mut in_bq = false;
                let mut n = self.tip;
                loop {
                    match self.nodes[n].kind {
                        Kind::List if list == usize::MAX => list = n,
                        Kind::BlockQuote => in_bq = true,
                        _ => {}
                    }
                    if n == 0 {
                        break;
                    }
                    n = self.nodes[n].parent;
                }
                if list != usize::MAX && in_bq {
                    self.nodes[list].src_end = (self.line_src_start + self.line.len()) as u32;
                }
            }
            let t = self.nodes[container].kind;
            let last_line_blank = self.blank
                && !(t == Kind::BlockQuote
                    || (t == Kind::CodeBlock && self.nodes[container].fenced)
                    || (t == Kind::Item
                        && self.nodes[container].first_child == NO_NODE
                        && self.nodes[container].start_line == self.line_number));
            let mut c = container;
            loop {
                self.nodes[c].last_line_blank = last_line_blank;
                if c == 0 {
                    break;
                }
                c = self.nodes[c].parent;
            }

            if accepts_lines(t) {
                self.add_line();
                if t == Kind::HtmlBlock && self.html_block_closes(container) {
                    let cur = container;
                    #[cfg(feature = "ast")]
                    {
                        self.nodes[cur].html_closed_by_cond = true;
                    }
                    self.finalize(cur);
                }
            } else if self.offset < self.line.len() && !self.blank {
                self.add_child(Kind::Paragraph);
                self.advance_next_nonspace();
                self.add_line();
            }
        }
        self.prev_blank = self.blank;
    }

    // ---- continuation per block kind ------------------------------------

    /// Returns 0 = matched, 1 = not matched, 2 = line fully consumed.
    fn continue_block(&mut self, c: usize) -> u8 {
        match self.nodes[c].kind {
            Kind::Document => 0,
            // Frontmatter is consumed in a pre-pass and born closed, so it is
            // never an open container here; treat as not-matched defensively.
            Kind::Frontmatter => 1,
            // Footnote definition: a block container with a fixed 4-space content
            // indent (one tab stop), lazy paragraph continuation, and blank lines
            // tolerated once it has content (mirrors a list item with padding 4).
            // Arm stays compiled (self-contained) so the match doesn't perturb.
            Kind::FootnoteDef => {
                if self.blank {
                    if self.nodes[c].first_child == NO_NODE {
                        1
                    } else {
                        self.advance_next_nonspace();
                        0
                    }
                } else if self.indent >= 4 {
                    self.advance_offset(4, true);
                    0
                } else {
                    1
                }
            }
            Kind::BlockQuote => {
                if !self.indented && peek(self.line, self.next_nonspace) == Some(b'>') {
                    self.advance_next_nonspace();
                    self.advance_offset(1, false);
                    if is_space_or_tab(peek(self.line, self.offset)) {
                        self.advance_offset(1, true);
                    }
                    // SPIKE (`ast`): the blockquote spans through its last
                    // `>`-marked line (incl. trailing blank `>` lines).
                    #[cfg(feature = "ast")]
                    {
                        self.nodes[c].src_end = (self.line_src_start + self.line.len()) as u32;
                    }
                    0
                } else {
                    1
                }
            }
            Kind::Item => {
                let (marker_offset, padding) = {
                    let ld = self.nodes[c].list.as_ref().unwrap();
                    (ld.marker_offset, ld.padding)
                };
                if self.blank {
                    if self.nodes[c].first_child == NO_NODE {
                        1
                    } else {
                        self.advance_next_nonspace();
                        0
                    }
                } else if self.indent >= marker_offset + padding {
                    self.advance_offset(marker_offset + padding, true);
                    0
                } else {
                    1
                }
            }
            Kind::List => 0,
            Kind::Paragraph => {
                if self.blank {
                    1
                } else {
                    0
                }
            }
            // A definition list stays open across blanks and intervening term
            // paragraphs; it closes only when a block that cannot be its child
            // forces it shut (handled by `add_child`) or at EOF.
            Kind::DefList => 0,
            // A description accepts lazy continuation lines like a paragraph, but
            // a blank line or a fresh `:` marker ends it (the marker then opens a
            // sibling description via `start_def_list`).
            Kind::DefDesc => {
                if self.blank || self.is_def_marker() {
                    1
                } else {
                    0
                }
            }
            // Term holders are closed the moment their description is attached, so
            // they are never re-entered as an open container.
            Kind::DefTerm => 1,
            // A container directive runs until its closing colon fence (a line of
            // ≥ the opening colon count); otherwise its content flows in as
            // markdown with no prefix stripping.
            // Arm stays compiled (keeps the hot continue match stable); without
            // the `directives` feature no `ContainerDirective` node is ever
            // created, so the body is gated out and the arm folds into `0`.
            Kind::ContainerDirective => {
                #[cfg(feature = "directives")]
                if !self.indented
                    && is_colon_close(self.line, self.next_nonspace, self.nodes[c].fence_len)
                {
                    let cur = c;
                    #[cfg(feature = "ast")]
                    {
                        self.nodes[cur].src_end = (self.line_src_start + self.line.len()) as u32;
                    }
                    self.finalize(cur);
                    return 2;
                }
                0
            }
            // Leaf directives are single-line, born closed; never re-entered.
            Kind::LeafDirective => 1,
            Kind::Heading | Kind::ThematicBreak => 1,
            // Definitions are inserted already-closed (never an open container).
            #[cfg(feature = "ast")]
            Kind::Definition => 1,
            Kind::CodeBlock => {
                if self.nodes[c].fenced {
                    let fc = self.nodes[c].fence_char;
                    let fl = self.nodes[c].fence_len;
                    let fo = self.nodes[c].fence_offset;
                    if !self.indented && is_closing_fence(self.line, self.next_nonspace, fc, fl) {
                        let cur = c;
                        // SPIKE (`ast`): a fenced block spans through its closing
                        // fence line (mdast includes the closing fence).
                        #[cfg(feature = "ast")]
                        {
                            self.nodes[cur].src_end =
                                (self.line_src_start + self.line.len()) as u32;
                        }
                        self.finalize(cur);
                        return 2;
                    }
                    // Remove up to fence_offset spaces of indentation.
                    let mut i = 0;
                    while i < fo && is_space_or_tab(peek(self.line, self.offset)) {
                        self.advance_offset(1, true);
                        i += 1;
                    }
                    0
                } else if self.indent >= CODE_INDENT {
                    self.advance_offset(CODE_INDENT, true);
                    0
                } else if self.blank {
                    self.advance_next_nonspace();
                    0
                } else {
                    1
                }
            }
            Kind::HtmlBlock => {
                let k = self.nodes[c].html_kind;
                if self.blank && (k == 6 || k == 7) {
                    1
                } else {
                    0
                }
            }
            // A table continues while rows keep coming: non-blank lines that
            // still look like a row (contain a pipe). Anything else closes it.
            #[cfg(feature = "gfm")]
            Kind::Table => u8::from(self.blank || !self.line[self.next_nonspace..].contains(&b'|')),
        }
    }

    // ---- block starts ---------------------------------------------------

    fn try_start(&mut self, which: usize, container: usize) -> u8 {
        match which {
            0 => self.start_block_quote(),
            1 => self.start_atx_heading(),
            2 => self.start_fenced_code(),
            3 => self.start_html_block(container),
            4 => self.start_setext_heading(container),
            5 => self.start_thematic_break(),
            6 => self.start_list_item(container),
            7 => self.start_indented_code(),
            // Footnote slot keeps main's index (8); its body is the only cfg'd
            // part — a no-op `=> 0` without the feature, so the dispatch order is
            // byte-identical to the proven-neutral baseline.
            #[cfg(feature = "footnotes")]
            8 => self.start_footnote_def(container),
            #[cfg(not(feature = "footnotes"))]
            8 => 0,
            // Deflist slot keeps main's index (9); its body is the only cfg'd
            // part — a no-op `=> 0` without the feature, so the dispatch order is
            // byte-identical to the proven-neutral baseline.
            #[cfg(feature = "deflist")]
            9 => self.start_def_list(container),
            #[cfg(not(feature = "deflist"))]
            9 => 0,
            // Directive slot keeps main's index (10); its body is the only cfg'd
            // part — a no-op `=> 0` without the feature, so the dispatch order is
            // byte-identical to the proven-neutral baseline.
            #[cfg(feature = "directives")]
            10 => self.start_directive(),
            #[cfg(not(feature = "directives"))]
            10 => 0,
            #[cfg(feature = "gfm")]
            11 => self.start_table(container),
            _ => 0,
        }
    }

    fn start_block_quote(&mut self) -> u8 {
        if !self.indented && peek(self.line, self.next_nonspace) == Some(b'>') {
            self.advance_next_nonspace();
            self.advance_offset(1, false);
            if is_space_or_tab(peek(self.line, self.offset)) {
                self.advance_offset(1, true);
            }
            self.close_unmatched_blocks();
            let bq = self.add_child(Kind::BlockQuote);
            #[cfg(feature = "ast")]
            {
                self.nodes[bq].src_end = (self.line_src_start + self.line.len()) as u32;
            }
            let _ = bq;
            1
        } else {
            0
        }
    }

    fn start_atx_heading(&mut self) -> u8 {
        if self.indented {
            return 0;
        }
        let rest = &self.line[self.next_nonspace..];
        let hashes = rest.iter().take_while(|&&b| b == b'#').count();
        if hashes == 0 || hashes > 6 {
            return 0;
        }
        match rest.get(hashes) {
            None | Some(b' ') | Some(b'\t') => {}
            _ => return 0,
        }
        self.advance_next_nonspace();
        self.advance_offset(hashes, false);
        self.close_unmatched_blocks();
        let h = self.add_child(Kind::Heading);
        self.nodes[h].level = hashes as u8;
        // Push the heading text into the shared buffer and record its range
        // (the source slices borrow `'a`, not `self`, so this is conflict-free).
        let after = std::str::from_utf8(&self.line[self.offset..]).unwrap_or("");
        let content = atx_content(after);
        let start = self.buf.len() as u32;
        // SPIKE (`ast`): the heading text maps 1:1 to its source slice; record the
        // breakpoint (content is a subslice of `self.line`).
        #[cfg(feature = "ast")]
        if !content.is_empty() {
            let coff = content.as_ptr() as usize - self.line.as_ptr() as usize;
            self.buf_segs
                .push((start, (self.line_src_start + coff) as u32));
        }
        self.buf.push_str(content);
        self.nodes[h].cstart = start;
        self.nodes[h].cend = self.buf.len() as u32;
        // SPIKE (`ast`): atx heading spans the whole line (markers included).
        #[cfg(feature = "ast")]
        {
            self.nodes[h].src_end = (self.line_src_start + self.line.len()) as u32;
        }
        self.advance_offset(self.line.len() - self.offset, false);
        2
    }

    fn start_fenced_code(&mut self) -> u8 {
        if self.indented {
            return 0;
        }
        let rest = &self.line[self.next_nonspace..];
        let Some((fence_char, fence_len)) = fence_opener(rest) else {
            return 0;
        };
        self.close_unmatched_blocks();
        let cb = self.add_child(Kind::CodeBlock);
        self.nodes[cb].fenced = true;
        self.nodes[cb].fence_char = fence_char;
        self.nodes[cb].fence_len = fence_len;
        self.nodes[cb].fence_offset = self.indent;
        self.advance_next_nonspace();
        self.advance_offset(fence_len, false);
        2
    }

    fn start_html_block(&mut self, container: usize) -> u8 {
        if self.indented || peek(self.line, self.next_nonspace) != Some(b'<') {
            return 0;
        }
        let rest = &self.line[self.next_nonspace..];
        let in_paragraph = self.nodes[container].kind == Kind::Paragraph;
        let Some(kind) = html_block_open(rest, in_paragraph) else {
            return 0;
        };
        self.close_unmatched_blocks();
        let h = self.add_child(Kind::HtmlBlock);
        self.nodes[h].html_kind = kind;
        // mdast html block starts at the line content region (incl. leading indent).
        #[cfg(feature = "ast")]
        {
            self.nodes[h].src_start = (self.line_src_start + self.offset) as u32;
        }
        2
    }

    fn start_setext_heading(&mut self, container: usize) -> u8 {
        if self.indented || self.nodes[container].kind != Kind::Paragraph {
            return 0;
        }
        let rest = &self.line[self.next_nonspace..];
        let Some(level) = setext_level(rest) else {
            return 0;
        };
        self.close_unmatched_blocks();
        // Strip leading ref defs from the paragraph; if nothing remains, no heading.
        let (s, e, csrc) = self.content_range(container);
        let (lead, off, defs, empty) = {
            let store: &str = if csrc { self.source } else { &self.buf };
            let sl = &store[s..e];
            let lead = sl.len() - sl.trim_start_matches(['\n', ' ', '\t']).len();
            let (off, defs) = take_ref_defs(&store[s + lead..e]);
            let empty = store[s + lead + off..e]
                .trim_matches(['\n', ' ', '\t'])
                .is_empty();
            (lead, off, defs, empty)
        };
        if empty {
            // Only reference definitions: not a heading. Leave them in place —
            // the paragraph's `finalize` registers/emits them (emitting here too
            // would duplicate the `definition` node).
            return 0;
        }
        // Becomes a heading: the defs are stripped from the heading content
        // below, so register/emit them now (finalize won't see them).
        #[cfg(feature = "ast")]
        self.emit_defs(container, defs);
        #[cfg(not(feature = "ast"))]
        for (label, dest, title) in defs {
            self.refmap
                .entry(crate::inline::normalize_label(&label).into_owned())
                .or_insert((dest, title));
        }
        // Reuse the paragraph node as the heading (its finalize trims the range).
        self.nodes[container].kind = Kind::Heading;
        self.nodes[container].level = level;
        self.nodes[container].cstart = (s + lead + off) as u32;
        // SPIKE (`ast`): a setext heading spans through its underline line.
        #[cfg(feature = "ast")]
        {
            self.nodes[container].src_end = (self.line_src_start + self.line.len()) as u32;
        }
        self.advance_offset(self.line.len() - self.offset, false);
        2
    }

    #[cfg(feature = "gfm")]
    /// GFM pipe table: the current line is a delimiter row and the open
    /// paragraph is a single-line header with a matching column count. Reuse the
    /// paragraph node as the table; the delimiter line (and later data rows)
    /// become table content via [`Self::add_line`].
    fn start_table(&mut self, container: usize) -> u8 {
        if self.indented || !self.opts.tables || self.nodes[container].kind != Kind::Paragraph {
            return 0;
        }
        let Some(ncols) = delim_row_cols(&self.line[self.next_nonspace..]) else {
            return 0;
        };
        let (s, e, csrc) = self.content_range(container);
        let header_ok = {
            let store: &str = if csrc { self.source } else { &self.buf };
            let header = store[s..e].trim_matches(['\n', ' ', '\t']);
            !header.is_empty() && !header.contains('\n') && count_cells(header.as_bytes()) == ncols
        };
        if !header_ok {
            return 0;
        }
        self.close_unmatched_blocks();
        self.nodes[container].kind = Kind::Table;
        2
    }

    fn start_thematic_break(&mut self) -> u8 {
        if self.indented {
            return 0;
        }
        if is_thematic_break(&self.line[self.next_nonspace..]) {
            self.close_unmatched_blocks();
            let tb = self.add_child(Kind::ThematicBreak);
            #[cfg(feature = "ast")]
            {
                self.nodes[tb].src_end = (self.line_src_start + self.line.len()) as u32;
            }
            let _ = tb;
            self.advance_offset(self.line.len() - self.offset, false);
            2
        } else {
            0
        }
    }

    fn start_list_item(&mut self, container: usize) -> u8 {
        if self.indented && self.nodes[container].kind != Kind::List {
            return 0;
        }
        let in_paragraph = self.nodes[container].kind == Kind::Paragraph;
        let Some(data) = self.parse_list_marker(in_paragraph) else {
            return 0;
        };
        self.close_unmatched_blocks();

        let tip_is_matching_list = self.nodes[self.tip].kind == Kind::List
            && self.nodes[self.tip]
                .list
                .as_ref()
                .is_some_and(|l| lists_match(l, &data));
        if !tip_is_matching_list {
            let l = self.add_child(Kind::List);
            self.nodes[l].list = Some(data.clone());
        }
        let item = self.add_child(Kind::Item);
        self.nodes[item].list = Some(data);
        // SPIKE (`ast`): default an empty item's end to the end of its marker
        // line (mdast keeps trailing spaces after the marker, e.g. "-   ").
        // Non-empty items override this with their last child's end.
        #[cfg(feature = "ast")]
        {
            self.nodes[item].src_end = (self.line_src_start + self.line.len()) as u32;
        }
        1
    }

    /// GFM footnote definition `[^label]: …`. A flow construct: it interrupts an
    /// open paragraph. Opens a `FootnoteDef` container; the remaining line content
    /// flows into it as a paragraph via the normal machinery.
    #[cfg(feature = "footnotes")]
    fn start_footnote_def(&mut self, _container: usize) -> u8 {
        if !self.opts.footnotes || self.indented {
            return 0;
        }
        let rest = &self.line[self.next_nonspace..];
        let Some((label, consumed)) = parse_footnote_label_def(rest) else {
            return 0;
        };
        self.close_unmatched_blocks();
        #[cfg(feature = "ast")]
        let src_start = (self.line_src_start + self.next_nonspace) as u32;
        let fnode = self.add_child(Kind::FootnoteDef);
        let identifier = label.to_lowercase();
        let fi = self.fn_defs.len() as u32;
        self.fn_defs.push(FnDef {
            label,
            identifier: identifier.clone(),
        });
        self.nodes[fnode].fn_idx = fi;
        self.footnote_ids.insert(identifier);
        #[cfg(feature = "ast")]
        {
            self.nodes[fnode].src_start = src_start;
            self.nodes[fnode].src_end = (self.line_src_start + self.line.len()) as u32;
        }
        // Consume `[^label]:` and a single optional following space/tab; the rest
        // of the line becomes the definition's first paragraph.
        self.advance_next_nonspace();
        self.advance_offset(consumed, false);
        if is_space_or_tab(peek(self.line, self.offset)) {
            self.advance_offset(1, true);
        }
        1
    }

    /// Is the current line a definition-list marker — `:` (≤3 spaces indent)
    /// followed by a space or tab?
    fn is_def_marker(&self) -> bool {
        !self.indented
            && peek(self.line, self.next_nonspace) == Some(b':')
            && is_space_or_tab(peek(self.line, self.next_nonspace + 1))
    }

    /// Definition list (pandoc / remark-definition-list). A `: definition` line
    /// attaches a description to the preceding term paragraph, opening a `<dl>`;
    /// further markers add more descriptions (and intervening paragraphs add more
    /// terms) to the same list. A blank line before the marker makes the
    /// description *loose* (its body is wrapped in `<p>`).
    #[cfg(feature = "deflist")]
    fn start_def_list(&mut self, container: usize) -> u8 {
        if !self.opts.deflist || self.indented || !self.is_def_marker() {
            return 0;
        }
        let loose = self.prev_blank;
        match self.nodes[container].kind {
            // An open paragraph directly above the marker is the term (tight). It
            // may already sit inside an open list (a second term/def group).
            Kind::Paragraph => {
                let para = container;
                let parent = self.nodes[para].parent;
                self.nodes[para].kind = Kind::DefTerm;
                self.nodes[para].open = false;
                self.tip = if self.nodes[parent].kind == Kind::DefList {
                    parent
                } else {
                    self.splice_def_list(parent, para)
                };
            }
            // The marker continues an open list (e.g. `Term\n: a\n: b`): another
            // description for the most recent term.
            Kind::DefList => {
                self.close_unmatched_blocks();
                self.tip = container;
            }
            // A blank line separated the term from the marker, so the term is the
            // container's (closed) last-child paragraph — a loose description.
            _ => {
                let Some(lc) = self.last_child(container) else {
                    return 0;
                };
                if self.nodes[lc].kind != Kind::Paragraph {
                    return 0;
                }
                self.nodes[lc].kind = Kind::DefTerm;
                self.nodes[lc].open = false;
                self.tip = self.splice_def_list(container, lc);
            }
        }
        let dd = self.add_child(Kind::DefDesc);
        self.nodes[dd].level = u8::from(loose);
        // Consume `:` and one optional space/tab; the rest of the line flows into
        // the description as its first content line.
        self.advance_next_nonspace();
        self.advance_offset(1, false);
        if is_space_or_tab(peek(self.line, self.offset)) {
            self.advance_offset(1, true);
        }
        2
    }

    /// Splice a fresh `DefList` into `parent`'s child list where `para` sits, then
    /// move `para` (already retyped as a `DefTerm`) inside it. Returns the list.
    #[cfg(feature = "deflist")]
    fn splice_def_list(&mut self, parent: usize, para: usize) -> usize {
        let dl = self.nodes.len();
        let mut node = Node::new(Kind::DefList, parent, self.nodes[para].start_line);
        #[cfg(feature = "ast")]
        {
            node.src_start = self.nodes[para].src_start;
            node.src_end = self.nodes[para].src_end;
        }
        node.first_child = para as u32;
        node.last_child = para as u32;
        node.next_sibling = self.nodes[para].next_sibling;
        self.nodes.push(node);
        let (dl32, para32) = (dl as u32, para as u32);
        if self.nodes[parent].first_child == para32 {
            self.nodes[parent].first_child = dl32;
        } else {
            let mut prev = self.nodes[parent].first_child;
            while self.nodes[prev as usize].next_sibling != para32 {
                prev = self.nodes[prev as usize].next_sibling;
            }
            self.nodes[prev as usize].next_sibling = dl32;
        }
        if self.nodes[parent].last_child == para32 {
            self.nodes[parent].last_child = dl32;
        }
        self.nodes[para].parent = dl;
        self.nodes[para].next_sibling = NO_NODE;
        dl
    }

    /// Block directives (remark-directive): a leaf `::name…` (one line) or a
    /// container `:::name…` … `:::`. The leading colon run selects the form (2 =
    /// leaf, ≥3 = container). The header (`name[label]{attrs}`) is parsed once and
    /// stashed in `directives`; the opening line must hold nothing but the header.
    #[cfg(feature = "directives")]
    fn start_directive(&mut self) -> u8 {
        if !self.opts.directives || self.indented {
            return 0;
        }
        let line = self.line;
        let rest = &line[self.next_nonspace..];
        let mut colons = 0;
        while colons < rest.len() && rest[colons] == b':' {
            colons += 1;
        }
        if colons < 2 {
            return 0;
        }
        let Some(h) = crate::directive::parse_header(&rest[colons..]) else {
            return 0;
        };
        // The remainder of the opening line must be blank (header only).
        if rest[colons + h.consumed..]
            .iter()
            .any(|b| !matches!(b, b' ' | b'\t' | b'\r' | b'\n'))
        {
            return 0;
        }
        let name =
            String::from_utf8_lossy(&rest[colons + h.name_start..colons + h.name_end]).into_owned();
        let base = self.line_src_start + self.next_nonspace + colons;
        let label = h.label.map(|(s, e)| ((base + s) as u32, (base + e) as u32));
        let di = self.directives.len() as u32;
        self.directives.push(DirData {
            name,
            attrs: h.attrs,
            label,
        });
        self.close_unmatched_blocks();
        #[cfg(feature = "ast")]
        let src_start = (self.line_src_start + self.next_nonspace) as u32;
        let kind = if colons == 2 {
            Kind::LeafDirective
        } else {
            Kind::ContainerDirective
        };
        let node = self.add_child(kind);
        self.nodes[node].fn_idx = di;
        if colons >= 3 {
            self.nodes[node].fenced = true;
            self.nodes[node].fence_char = b':';
            self.nodes[node].fence_len = colons;
        }
        #[cfg(feature = "ast")]
        {
            self.nodes[node].src_start = src_start;
            self.nodes[node].src_end = (self.line_src_start + self.line.len()) as u32;
        }
        // Consume the whole opening line so it never becomes child content.
        self.advance_offset(self.line.len() - self.offset, false);
        if colons == 2 { 2 } else { 1 }
    }

    fn start_indented_code(&mut self) -> u8 {
        if self.indented && self.nodes[self.tip].kind != Kind::Paragraph && !self.blank {
            // mdast indented code starts at the line content region (incl. indent).
            // If a container marker left a tab partially consumed, the start
            // rounds up past that tab byte (a position can't sit inside a byte).
            #[cfg(feature = "ast")]
            let content_start =
                self.line_src_start + self.offset + self.partially_consumed_tab as usize;
            self.advance_offset(CODE_INDENT, true);
            self.close_unmatched_blocks();
            let cb = self.add_child(Kind::CodeBlock);
            self.nodes[cb].fenced = false;
            #[cfg(feature = "ast")]
            {
                self.nodes[cb].src_start = content_start as u32;
            }
            2
        } else {
            0
        }
    }

    fn parse_list_marker(&mut self, in_paragraph: bool) -> Option<ListData> {
        if self.indent >= 4 {
            return None;
        }
        let rest = &self.line[self.next_nonspace..];
        let mut data = ListData {
            ordered: false,
            bullet: 0,
            start: 0,
            delimiter: 0,
            padding: 0,
            marker_offset: self.indent,
            tight: true,
            #[cfg(feature = "ast")]
            spread: false,
        };
        let marker_width;
        match rest.first() {
            Some(&c @ (b'-' | b'+' | b'*')) => {
                data.bullet = c;
                marker_width = 1;
            }
            Some(b'0'..=b'9') => {
                let digits = rest.iter().take_while(|b| b.is_ascii_digit()).count();
                if digits > 9 {
                    return None;
                }
                match rest.get(digits) {
                    Some(&d @ (b'.' | b')')) => {
                        data.ordered = true;
                        data.start = std::str::from_utf8(&rest[..digits])
                            .unwrap()
                            .parse()
                            .unwrap();
                        data.delimiter = d;
                        marker_width = digits + 1;
                    }
                    _ => return None,
                }
            }
            _ => return None,
        }
        // Must be followed by a space/tab or end of line.
        match rest.get(marker_width) {
            None | Some(b' ') | Some(b'\t') => {}
            _ => return None,
        }
        // Interrupting a paragraph: ordered must start at 1, and not be blank.
        if in_paragraph {
            if data.ordered && data.start != 1 {
                return None;
            }
            let after_blank = rest[marker_width..]
                .iter()
                .all(|&b| b == b' ' || b == b'\t');
            if after_blank {
                return None;
            }
        }

        self.advance_next_nonspace();
        self.advance_offset(marker_width, true);
        let spaces_start_col = self.column;
        let spaces_start_offset = self.offset;
        loop {
            self.advance_offset(1, true);
            if self.column - spaces_start_col >= 5 || !is_space_or_tab(peek(self.line, self.offset))
            {
                break;
            }
        }
        let blank_item = peek(self.line, self.offset).is_none();
        let spaces = self.column - spaces_start_col;
        if !(1..5).contains(&spaces) || blank_item {
            data.padding = marker_width + 1;
            self.column = spaces_start_col;
            self.offset = spaces_start_offset;
            if is_space_or_tab(peek(self.line, self.offset)) {
                self.advance_offset(1, true);
            }
        } else {
            data.padding = marker_width + spaces;
        }
        Some(data)
    }

    fn html_block_closes(&self, c: usize) -> bool {
        let k = self.nodes[c].html_kind;
        if !(1..=5).contains(&k) {
            return false;
        }
        let line = std::str::from_utf8(&self.line[self.offset..]).unwrap_or("");
        match k {
            1 => {
                let l = line.to_ascii_lowercase();
                l.contains("</script>")
                    || l.contains("</pre>")
                    || l.contains("</style>")
                    || l.contains("</textarea>")
            }
            2 => line.contains("-->"),
            3 => line.contains("?>"),
            4 => line.contains('>'),
            5 => line.contains("]]>"),
            _ => false,
        }
    }
}

// Keep main's exact index map regardless of `footnotes` — the footnote slot
// (8) stays counted; without the feature its `try_start` arm is a no-op `=> 0`.
// (Renumbering the slots to drop the footnote index perturbs the hot block-start
// dispatch — measured +3% on the default path — so we don't.)
const NUM_STARTS: usize = if cfg!(feature = "gfm") { 12 } else { 11 };

/// Could a line whose first non-space byte is `c` begin a block other than a
/// paragraph? (`#` ATX, `>` quote, `` ` ``/`~` fence, `*+-_` thematic/list,
/// `=`/`-` setext, `<` HTML, digit ordered list.) Used to skip block-start
/// matching on plain prose lines.
fn maybe_special(c: Option<u8>) -> bool {
    matches!(
        c,
        Some(b'#' | b'>' | b'`' | b'~' | b'*' | b'+' | b'-' | b'_' | b'=' | b'<' | b'0'..=b'9')
    )
}

/// Drop a single trailing `\r` so CRLF fence lines test like LF ones.
fn trim_cr(line: &[u8]) -> &[u8] {
    if let [rest @ .., b'\r'] = line {
        rest
    } else {
        line
    }
}

/// Parse a GFM footnote-definition opener `[^label]:` at the start of `line`.
/// Returns `(label, bytes_through_colon)`. The label is non-empty, contains no
/// unescaped brackets and no whitespace (backslash escapes are kept verbatim, as
/// remark does), and must be immediately followed by `]:`.
#[cfg(feature = "footnotes")]
fn parse_footnote_label_def(line: &[u8]) -> Option<(String, usize)> {
    if line.len() < 4 || line[0] != b'[' || line[1] != b'^' {
        return None;
    }
    let start = 2;
    let mut i = start;
    while i < line.len() {
        match line[i] {
            b']' => break,
            b'[' => return None,
            b' ' | b'\t' | b'\n' | b'\r' => return None,
            b'\\' if i + 1 < line.len() => i += 2, // escaped char kept in the label
            _ => i += 1,
        }
    }
    if i >= line.len() || line[i] != b']' || i == start || line.get(i + 1) != Some(&b':') {
        return None;
    }
    let label = std::str::from_utf8(&line[start..i]).ok()?.to_string();
    Some((label, i + 2))
}

/// A frontmatter fence: exactly three `marker` bytes then only spaces/tabs
/// (a trailing `\r` already trimmed). Four or more markers is not a fence.
fn is_fm_fence(line: &[u8], marker: u8) -> bool {
    line.len() >= 3
        && line[0] == marker
        && line[1] == marker
        && line[2] == marker
        && line[3..].iter().all(|&b| b == b' ' || b == b'\t')
}

fn can_contain(parent: Kind, child: Kind) -> bool {
    match parent {
        Kind::Document | Kind::BlockQuote | Kind::Item => child != Kind::Item,
        #[cfg(feature = "footnotes")]
        Kind::FootnoteDef => child != Kind::Item,
        Kind::ContainerDirective => child != Kind::Item,
        Kind::List => child == Kind::Item,
        // A definition list holds term holders and descriptions; a plain
        // paragraph child is a pending term candidate (a dangling one is evicted
        // when the list finalizes).
        Kind::DefList => matches!(child, Kind::DefTerm | Kind::DefDesc | Kind::Paragraph),
        _ => false,
    }
}

fn accepts_lines(kind: Kind) -> bool {
    if matches!(
        kind,
        Kind::Paragraph | Kind::CodeBlock | Kind::HtmlBlock | Kind::DefDesc
    ) {
        return true;
    }
    #[cfg(feature = "gfm")]
    if kind == Kind::Table {
        return true;
    }
    false
}

#[cfg(feature = "gfm")]
/// Trim ASCII spaces and tabs from both ends of a byte slice.
fn trim_sp(mut s: &[u8]) -> &[u8] {
    while let [b' ' | b'\t', rest @ ..] = s {
        s = rest;
    }
    while let [rest @ .., b' ' | b'\t'] = s {
        s = rest;
    }
    s
}

#[cfg(feature = "gfm")]
/// Number of cells in a GFM table row: pipe-separated, honoring `\|` escapes and
/// dropping a single optional leading/trailing pipe.
fn count_cells(line: &[u8]) -> usize {
    let t = trim_sp(line);
    if t.is_empty() {
        return 0;
    }
    let mut parts = 1; // cells = unescaped pipes + 1
    let mut esc = false;
    for &b in t {
        if esc {
            esc = false;
        } else if b == b'\\' {
            esc = true;
        } else if b == b'|' {
            parts += 1;
        }
    }
    // A leading/trailing pipe contributes an empty outer cell that doesn't count.
    parts -= (t[0] == b'|') as usize;
    parts -= (t[t.len() - 1] == b'|' && t.len() > 1) as usize;
    parts
}

#[cfg(feature = "gfm")]
/// If `line` is a valid GFM delimiter row (cells of `:?-+:?`, with at least one
/// pipe to disambiguate it from a setext underline), return the column count.
fn delim_row_cols(line: &[u8]) -> Option<usize> {
    let mut t = trim_sp(line);
    if !t.contains(&b'|') {
        return None;
    }
    if t.first() == Some(&b'|') {
        t = &t[1..];
    }
    if t.last() == Some(&b'|') {
        t = &t[..t.len() - 1];
    }
    let mut cols = 0;
    for cell in t.split(|&b| b == b'|') {
        let mut inner = trim_sp(cell);
        if inner.first() == Some(&b':') {
            inner = &inner[1..];
        }
        if inner.last() == Some(&b':') {
            inner = &inner[..inner.len() - 1];
        }
        if inner.is_empty() || !inner.iter().all(|&b| b == b'-') {
            return None;
        }
        cols += 1;
    }
    Some(cols)
}

fn lists_match(a: &ListData, b: &ListData) -> bool {
    a.ordered == b.ordered
        && if a.ordered {
            a.delimiter == b.delimiter
        } else {
            a.bullet == b.bullet
        }
}

// ---- line helpers --------------------------------------------------------

fn atx_content(after: &str) -> &str {
    let c = after.trim_matches([' ', '\t']);
    let t = c.trim_end_matches('#');
    if t.len() == c.len() {
        c
    } else if t.is_empty() {
        ""
    } else if t.ends_with([' ', '\t']) {
        t.trim_end_matches([' ', '\t'])
    } else {
        c
    }
}

fn setext_level(rest: &[u8]) -> Option<u8> {
    let end = rest
        .iter()
        .rposition(|&b| b != b' ' && b != b'\t')
        .map(|p| p + 1)
        .unwrap_or(0);
    let core = &rest[..end];
    if core.is_empty() {
        return None;
    }
    let c = core[0];
    if (c == b'=' || c == b'-') && core.iter().all(|&b| b == c) {
        Some(if c == b'=' { 1 } else { 2 })
    } else {
        None
    }
}

fn is_thematic_break(rest: &[u8]) -> bool {
    let mut ch = 0u8;
    let mut count = 0;
    for &b in rest {
        match b {
            b' ' | b'\t' => {}
            b'-' | b'_' | b'*' => {
                if ch == 0 {
                    ch = b;
                } else if b != ch {
                    return false;
                }
                count += 1;
            }
            b'\n' => break,
            _ => return false,
        }
    }
    count >= 3
}

fn fence_opener(rest: &[u8]) -> Option<(u8, usize)> {
    let c = *rest.first()?;
    if c != b'`' && c != b'~' {
        return None;
    }
    let len = rest.iter().take_while(|&&b| b == c).count();
    if len < 3 {
        return None;
    }
    // Backtick info strings may not contain backticks.
    if c == b'`'
        && rest[len..]
            .iter()
            .take_while(|&&b| b != b'\n')
            .any(|&b| b == b'`')
    {
        return None;
    }
    Some((c, len))
}

fn is_closing_fence(line: &[u8], from: usize, fence_char: u8, fence_len: usize) -> bool {
    let rest = &line[from..];
    let run = rest.iter().take_while(|&&b| b == fence_char).count();
    if run < fence_len {
        return false;
    }
    rest[run..]
        .iter()
        .all(|&b| b == b' ' || b == b'\t' || b == b'\n')
}

/// A container-directive closing fence: a run of `≥ max(fence_len, 3)` colons
/// followed only by whitespace.
#[cfg(feature = "directives")]
fn is_colon_close(line: &[u8], from: usize, fence_len: usize) -> bool {
    let rest = &line[from..];
    let run = rest.iter().take_while(|&&b| b == b':').count();
    if run < fence_len.max(3) {
        return false;
    }
    rest[run..]
        .iter()
        .all(|&b| b == b' ' || b == b'\t' || b == b'\r' || b == b'\n')
}

/// Byte length of `s` after stripping a trailing `(\n *)+` run (HTML-block
/// literal normalization).
fn html_trim_end(s: &str) -> usize {
    let bytes = s.as_bytes();
    let mut end = s.len();
    loop {
        let mut e = end;
        while e > 0 && bytes[e - 1] == b' ' {
            e -= 1;
        }
        if e > 0 && bytes[e - 1] == b'\n' {
            end = e - 1;
        } else {
            break;
        }
    }
    end
}

/// Byte length of `s` (whose lines each end in `\n`) to keep after dropping
/// trailing blank lines — content + the last non-blank line's newline.
fn code_indented_end(s: &str) -> usize {
    let bytes = s.as_bytes();
    let mut keep = 0;
    let mut start = 0;
    let mut i = 0;
    while i <= bytes.len() {
        if i == bytes.len() || bytes[i] == b'\n' {
            if !s[start..i].trim_matches([' ', '\t']).is_empty() {
                keep = (i + 1).min(s.len());
            }
            start = i + 1;
        }
        i += 1;
    }
    keep
}

// ---- HTML block start conditions ----------------------------------------

const HTML_BLOCK_NAMES: &[&str] = &[
    "address",
    "article",
    "aside",
    "base",
    "basefont",
    "blockquote",
    "body",
    "caption",
    "center",
    "col",
    "colgroup",
    "dd",
    "details",
    "dialog",
    "dir",
    "div",
    "dl",
    "dt",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "frame",
    "frameset",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "head",
    "header",
    "hr",
    "html",
    "iframe",
    "legend",
    "li",
    "link",
    "main",
    "menu",
    "menuitem",
    "nav",
    "noframes",
    "ol",
    "optgroup",
    "option",
    "p",
    "param",
    "section",
    "summary",
    "table",
    "tbody",
    "td",
    "tfoot",
    "th",
    "thead",
    "title",
    "tr",
    "track",
    "ul",
];

fn html_block_open(rest: &[u8], in_paragraph: bool) -> Option<u8> {
    let s = std::str::from_utf8(rest).unwrap_or("");
    let lower = s.to_ascii_lowercase();
    // 1: <script | <pre | <style | <textarea
    for tag in ["script", "pre", "style", "textarea"] {
        let open = format!("<{tag}");
        if lower.starts_with(&open) {
            let after = lower.as_bytes().get(open.len());
            if matches!(
                after,
                None | Some(b' ') | Some(b'\t') | Some(b'\n') | Some(b'>')
            ) {
                return Some(1);
            }
        }
    }
    // 2: <!--
    if s.starts_with("<!--") {
        return Some(2);
    }
    // 3: <?
    if s.starts_with("<?") {
        return Some(3);
    }
    // 4: <! + ASCII letter
    if s.starts_with("<!") && s.as_bytes().get(2).is_some_and(u8::is_ascii_alphabetic) {
        return Some(4);
    }
    // 5: <![CDATA[
    if s.starts_with("<![CDATA[") {
        return Some(5);
    }
    // 6: block tag name
    let body = lower.strip_prefix("</").or_else(|| lower.strip_prefix('<'));
    if let Some(b) = body {
        let name: String = b
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect();
        if !name.is_empty() && HTML_BLOCK_NAMES.contains(&name.as_str()) {
            let after = &b[name.len()..];
            if after.is_empty()
                || after.starts_with([' ', '\t'])
                || after.starts_with('>')
                || after.starts_with("/>")
                || after.starts_with('\n')
            {
                return Some(6);
            }
        }
    }
    // 7: complete open or closing tag, alone on the line (not interrupting a paragraph)
    if !in_paragraph
        && let Some(end) = full_tag(rest)
        && rest[end..]
            .iter()
            .all(|&b| b == b' ' || b == b'\t' || b == b'\n')
    {
        let name_ok = !s.starts_with("<script")
            && !s.starts_with("<pre")
            && !s.starts_with("<style")
            && !s.starts_with("<textarea");
        if name_ok {
            return Some(7);
        }
    }
    None
}

/// A complete open/closing tag at the start of `rest`; returns its end index.
fn full_tag(rest: &[u8]) -> Option<usize> {
    crate::inline::raw_tag_len(rest)
}
