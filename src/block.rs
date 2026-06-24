//! Block parser — container-aware and incremental (CommonMark §3–§5).
//!
//! A faithful port of the reference algorithm: each line is matched against
//! the open-block tree (continuation), then against block starts (new
//! containers/leaves), then its text is added to the open leaf. The result is
//! a node arena ([`Tree`]) that the renderer walks. Inline content is parsed
//! lazily at render time.

use crate::inline::{take_ref_defs, RefMap};

const CODE_INDENT: usize = 4;

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
}

pub struct Node {
    pub kind: Kind,
    pub children: Vec<usize>,
    pub parent: usize,
    open: bool,
    last_line_blank: bool,
    start_line: u32,
    /// Accumulated raw text (paragraph/heading inline source; code/html literal).
    pub content: String,
    pub level: u8,
    pub fenced: bool,
    fence_char: u8,
    fence_len: usize,
    fence_offset: usize,
    pub info: String,
    html_kind: u8,
    pub list: Option<ListData>,
}

impl Node {
    fn new(kind: Kind, parent: usize, line: u32) -> Self {
        Node {
            kind,
            children: Vec::new(),
            parent,
            open: true,
            last_line_blank: false,
            start_line: line,
            content: String::new(),
            level: 0,
            fenced: false,
            fence_char: 0,
            fence_len: 0,
            fence_offset: 0,
            info: String::new(),
            html_kind: 0,
            list: None,
        }
    }
}

pub struct Tree {
    pub nodes: Vec<Node>,
    pub root: usize,
    pub refmap: RefMap,
    pub source_len: usize,
}

/// Parse `src` into a block tree plus its link reference definitions.
pub fn parse(src: &str) -> Tree {
    Parser::new().parse(src)
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
    // line state — borrows the source line (no per-line allocation)
    line: &'a [u8],
    line_number: u32,
    offset: usize,
    column: usize,
    next_nonspace: usize,
    next_nonspace_column: usize,
    indent: usize,
    indented: bool,
    blank: bool,
    partially_consumed_tab: bool,
}

impl<'a> Parser<'a> {
    fn new() -> Self {
        let root = Node::new(Kind::Document, 0, 0);
        Parser {
            nodes: vec![root],
            tip: 0,
            oldtip: 0,
            last_matched_container: 0,
            all_closed: true,
            refmap: RefMap::new(),
            line: &[],
            line_number: 0,
            offset: 0,
            column: 0,
            next_nonspace: 0,
            next_nonspace_column: 0,
            indent: 0,
            indented: false,
            blank: false,
            partially_consumed_tab: false,
        }
    }

    fn last_child(&self, n: usize) -> Option<usize> {
        self.nodes[n].children.last().copied()
    }

    // ---- line position helpers ------------------------------------------

    fn find_next_nonspace(&mut self) {
        let mut i = self.offset;
        let mut cols = self.column;
        loop {
            match peek(&self.line, i) {
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
        self.blank = matches!(peek(&self.line, i), None | Some(b'\n'));
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
            match peek(&self.line, self.offset) {
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
        self.nodes.push(Node::new(kind, parent, self.line_number));
        self.nodes[parent].children.push(idx);
        self.tip = idx;
        idx
    }

    fn add_line(&mut self) {
        if self.partially_consumed_tab {
            self.offset += 1;
            let chars_to_tab = 4 - (self.column % 4);
            for _ in 0..chars_to_tab {
                self.nodes[self.tip].content.push(' ');
            }
        }
        let rest = &self.line[self.offset..];
        // line never contains an embedded NUL; push as UTF-8.
        self.nodes[self.tip]
            .content
            .push_str(std::str::from_utf8(rest).unwrap_or(""));
        self.nodes[self.tip].content.push('\n');
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

    fn finalize(&mut self, idx: usize) {
        let parent = self.nodes[idx].parent;
        self.nodes[idx].open = false;

        match self.nodes[idx].kind {
            Kind::Paragraph => {
                let mut content = std::mem::take(&mut self.nodes[idx].content);
                // Strip leading whitespace and any link reference definitions.
                let lead = content.len() - content.trim_start_matches(['\n', ' ', '\t']).len();
                let (off, defs) = take_ref_defs(&content[lead..]);
                for (label, dest, title) in defs {
                    self.refmap.entry(label).or_insert((dest, title));
                }
                content.drain(..lead + off);
                if content.trim_matches(['\n', ' ', '\t']).is_empty() {
                    // Pure reference definitions: drop the empty paragraph.
                    self.unlink(idx);
                } else {
                    // Stored content trims surrounding newlines only (in place).
                    let end = content.trim_end_matches('\n').len();
                    content.truncate(end);
                    let nlead = content.len() - content.trim_start_matches('\n').len();
                    content.drain(..nlead);
                    self.nodes[idx].content = content;
                }
            }
            Kind::Heading => {
                let content = &mut self.nodes[idx].content;
                let end = content.trim_end_matches(['\n', ' ', '\t']).len();
                content.truncate(end);
                let lead = content.len() - content.trim_start_matches(['\n', ' ', '\t']).len();
                content.drain(..lead);
            }
            Kind::CodeBlock => {
                if self.nodes[idx].fenced {
                    let content = std::mem::take(&mut self.nodes[idx].content);
                    let nl = content.find('\n').unwrap_or(content.len());
                    let first = &content[..nl];
                    let rest = content.get(nl + 1..).unwrap_or("");
                    self.nodes[idx].info = unescape_info(first.trim());
                    self.nodes[idx].content = rest.to_string();
                } else {
                    let content = std::mem::take(&mut self.nodes[idx].content);
                    self.nodes[idx].content = trim_trailing_blank_lines(&content);
                }
            }
            Kind::HtmlBlock => {
                let content = std::mem::take(&mut self.nodes[idx].content);
                self.nodes[idx].content = trim_html_block(&content);
            }
            Kind::List => {
                let tight = self.compute_tight(idx);
                if let Some(ld) = &mut self.nodes[idx].list {
                    ld.tight = tight;
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
        self.nodes[parent].children.retain(|&c| c != idx);
    }

    fn compute_tight(&self, list: usize) -> bool {
        let items = &self.nodes[list].children;
        for (ii, &item) in items.iter().enumerate() {
            // blank line at end of an item that is not the last → loose
            if self.ends_with_blank_line(item) && ii + 1 < items.len() {
                return false;
            }
            let subs = &self.nodes[item].children;
            for (si, &sub) in subs.iter().enumerate() {
                let last_in_item = si + 1 == subs.len();
                if self.ends_with_blank_line(sub) && !(ii + 1 == items.len() && last_in_item) {
                    return false;
                }
            }
        }
        true
    }

    fn ends_with_blank_line(&self, mut idx: usize) -> bool {
        loop {
            if self.nodes[idx].last_line_blank {
                return true;
            }
            if matches!(self.nodes[idx].kind, Kind::List | Kind::Item) {
                match self.nodes[idx].children.last() {
                    Some(&c) => idx = c,
                    None => return false,
                }
            } else {
                return false;
            }
        }
    }

    // ---- main loop ------------------------------------------------------

    fn parse(mut self, src: &'a str) -> Tree {
        // Rough upper bound on node count, to avoid arena reallocations.
        self.nodes.reserve(src.len() / 32);
        for line in split_lines(src) {
            self.incorporate_line(line);
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
        }
    }

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
                2 => return, // line fully consumed (code block)
                _ => unreachable!(),
            }
            if !all_matched {
                container = self.nodes[container].parent;
                break;
            }
        }

        self.all_closed = container == self.oldtip;
        self.last_matched_container = container;

        let mut matched_leaf =
            self.nodes[container].kind != Kind::Paragraph && accepts_lines(self.nodes[container].kind);

        // Phase 2: look for new block starts.
        while !matched_leaf {
            self.find_next_nonspace();
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
            if self.blank {
                if let Some(lc) = self.last_child(container) {
                    self.nodes[lc].last_line_blank = true;
                }
            }
            let t = self.nodes[container].kind;
            let last_line_blank = self.blank
                && !(t == Kind::BlockQuote
                    || (t == Kind::CodeBlock && self.nodes[container].fenced)
                    || (t == Kind::Item
                        && self.nodes[container].children.is_empty()
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
                    self.finalize(cur);
                }
            } else if self.offset < self.line.len() && !self.blank {
                self.add_child(Kind::Paragraph);
                self.advance_next_nonspace();
                self.add_line();
            }
        }
    }

    // ---- continuation per block kind ------------------------------------

    /// Returns 0 = matched, 1 = not matched, 2 = line fully consumed.
    fn continue_block(&mut self, c: usize) -> u8 {
        match self.nodes[c].kind {
            Kind::Document => 0,
            Kind::BlockQuote => {
                if !self.indented && peek(&self.line, self.next_nonspace) == Some(b'>') {
                    self.advance_next_nonspace();
                    self.advance_offset(1, false);
                    if is_space_or_tab(peek(&self.line, self.offset)) {
                        self.advance_offset(1, true);
                    }
                    0
                } else {
                    1
                }
            }
            Kind::Item => {
                let ld = self.nodes[c].list.clone().unwrap();
                if self.blank {
                    if self.nodes[c].children.is_empty() {
                        1
                    } else {
                        self.advance_next_nonspace();
                        0
                    }
                } else if self.indent >= ld.marker_offset + ld.padding {
                    self.advance_offset(ld.marker_offset + ld.padding, true);
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
            Kind::Heading | Kind::ThematicBreak => 1,
            Kind::CodeBlock => {
                if self.nodes[c].fenced {
                    let fc = self.nodes[c].fence_char;
                    let fl = self.nodes[c].fence_len;
                    let fo = self.nodes[c].fence_offset;
                    if !self.indented && is_closing_fence(&self.line, self.next_nonspace, fc, fl) {
                        let cur = c;
                        self.finalize(cur);
                        return 2;
                    }
                    // Remove up to fence_offset spaces of indentation.
                    let mut i = 0;
                    while i < fo && is_space_or_tab(peek(&self.line, self.offset)) {
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
            _ => 0,
        }
    }

    fn start_block_quote(&mut self) -> u8 {
        if !self.indented && peek(&self.line, self.next_nonspace) == Some(b'>') {
            self.advance_next_nonspace();
            self.advance_offset(1, false);
            if is_space_or_tab(peek(&self.line, self.offset)) {
                self.advance_offset(1, true);
            }
            self.close_unmatched_blocks();
            self.add_child(Kind::BlockQuote);
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
        let after = std::str::from_utf8(&self.line[self.offset..]).unwrap_or("");
        self.nodes[h].content = atx_content(after).to_string();
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
        if self.indented || peek(&self.line, self.next_nonspace) != Some(b'<') {
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
        let content = std::mem::take(&mut self.nodes[container].content);
        let trimmed = content.trim_start_matches(['\n', ' ', '\t']);
        let (off, defs) = take_ref_defs(trimmed);
        for (label, dest, title) in defs {
            self.refmap.entry(label).or_insert((dest, title));
        }
        let body = trimmed[off..].trim_matches(['\n', ' ', '\t']);
        if body.is_empty() {
            self.nodes[container].content = content;
            return 0;
        }
        // Reuse the paragraph node as the heading.
        self.nodes[container].kind = Kind::Heading;
        self.nodes[container].level = level;
        self.nodes[container].content = body.to_string();
        self.advance_offset(self.line.len() - self.offset, false);
        2
    }

    fn start_thematic_break(&mut self) -> u8 {
        if self.indented {
            return 0;
        }
        if is_thematic_break(&self.line[self.next_nonspace..]) {
            self.close_unmatched_blocks();
            self.add_child(Kind::ThematicBreak);
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
        1
    }

    fn start_indented_code(&mut self) -> u8 {
        if self.indented && self.nodes[self.tip].kind != Kind::Paragraph && !self.blank {
            self.advance_offset(CODE_INDENT, true);
            self.close_unmatched_blocks();
            let cb = self.add_child(Kind::CodeBlock);
            self.nodes[cb].fenced = false;
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
                        data.start = std::str::from_utf8(&rest[..digits]).unwrap().parse().unwrap();
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
            let after_blank = rest[marker_width..].iter().all(|&b| b == b' ' || b == b'\t');
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
            if self.column - spaces_start_col >= 5 || !is_space_or_tab(peek(&self.line, self.offset))
            {
                break;
            }
        }
        let blank_item = peek(&self.line, self.offset).is_none();
        let spaces = self.column - spaces_start_col;
        if spaces >= 5 || spaces < 1 || blank_item {
            data.padding = marker_width + 1;
            self.column = spaces_start_col;
            self.offset = spaces_start_offset;
            if is_space_or_tab(peek(&self.line, self.offset)) {
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
                l.contains("</script>") || l.contains("</pre>") || l.contains("</style>")
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

const NUM_STARTS: usize = 8;

fn can_contain(parent: Kind, child: Kind) -> bool {
    match parent {
        Kind::Document | Kind::BlockQuote | Kind::Item => child != Kind::Item,
        Kind::List => child == Kind::Item,
        _ => false,
    }
}

fn accepts_lines(kind: Kind) -> bool {
    matches!(kind, Kind::Paragraph | Kind::CodeBlock | Kind::HtmlBlock)
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

fn split_lines(src: &str) -> Vec<&[u8]> {
    if src.is_empty() {
        return Vec::new();
    }
    let bytes = src.as_bytes();
    let mut lines = Vec::new();
    let mut start = 0;
    for i in 0..bytes.len() {
        if bytes[i] == b'\n' {
            lines.push(&bytes[start..i]);
            start = i + 1;
        }
    }
    if start < bytes.len() {
        lines.push(&bytes[start..]);
    }
    lines
}

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
    if c == b'`' && rest[len..].iter().take_while(|&&b| b != b'\n').any(|&b| b == b'`') {
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
    rest[run..].iter().all(|&b| b == b' ' || b == b'\t' || b == b'\n')
}

fn unescape_info(s: &str) -> String {
    // The info string's backslash escapes and entities are resolved here; the
    // renderer takes its first word as the language class.
    crate::inline::unescape_string(s)
}

/// Strip a trailing `(\n *)+` run (HTML-block literal normalization).
fn trim_html_block(s: &str) -> String {
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
    s[..end].to_string()
}

fn trim_trailing_blank_lines(s: &str) -> String {
    let mut lines: Vec<&str> = s.split('\n').collect();
    if lines.last() == Some(&"") {
        lines.pop();
    }
    while lines
        .last()
        .is_some_and(|l| l.trim_matches([' ', '\t']).is_empty())
    {
        lines.pop();
    }
    if lines.is_empty() {
        String::new()
    } else {
        let mut out = lines.join("\n");
        out.push('\n');
        out
    }
}

// ---- HTML block start conditions ----------------------------------------

const HTML_BLOCK_NAMES: &[&str] = &[
    "address", "article", "aside", "base", "basefont", "blockquote", "body", "caption", "center",
    "col", "colgroup", "dd", "details", "dialog", "dir", "div", "dl", "dt", "fieldset",
    "figcaption", "figure", "footer", "form", "frame", "frameset", "h1", "h2", "h3", "h4", "h5",
    "h6", "head", "header", "hr", "html", "iframe", "legend", "li", "link", "main", "menu",
    "menuitem", "nav", "noframes", "ol", "optgroup", "option", "p", "param", "section", "summary",
    "table", "tbody", "td", "tfoot", "th", "thead", "title", "tr", "track", "ul",
];

fn html_block_open(rest: &[u8], in_paragraph: bool) -> Option<u8> {
    let s = std::str::from_utf8(rest).unwrap_or("");
    let lower = s.to_ascii_lowercase();
    // 1: <script | <pre | <style | <textarea
    for tag in ["script", "pre", "style", "textarea"] {
        let open = format!("<{tag}");
        if lower.starts_with(&open) {
            let after = lower.as_bytes().get(open.len());
            if matches!(after, None | Some(b' ') | Some(b'\t') | Some(b'\n') | Some(b'>')) {
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
        let name: String = b.chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
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
    if !in_paragraph {
        if let Some(end) = full_tag(rest) {
            if rest[end..].iter().all(|&b| b == b' ' || b == b'\t' || b == b'\n') {
                let name_ok = !s.starts_with("<script")
                    && !s.starts_with("<pre")
                    && !s.starts_with("<style")
                    && !s.starts_with("<textarea");
                if name_ok {
                    return Some(7);
                }
            }
        }
    }
    None
}

/// A complete open/closing tag at the start of `rest`; returns its end index.
fn full_tag(rest: &[u8]) -> Option<usize> {
    crate::inline::raw_tag_len(rest)
}
