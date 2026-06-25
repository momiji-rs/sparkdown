//! Inline parser.
//!
//! A left-to-right scan over a block's raw text builds a doubly-linked node
//! list; emphasis and links — which resolve non-locally — are handled with a
//! delimiter stack and resolved in later phases before the list is rendered.
//!
//! Implemented: backslash escapes, hard/soft line breaks, entity & numeric
//! references, code spans, autolinks, emphasis/strong, and links & images
//! (inline + reference, the latter via the [`RefMap`]).

use crate::entities::{named, remap_numeric};
use crate::render::escape_html;
use crate::scan::{find_emph, find_inline, find_stream};
use std::borrow::Cow;
use std::collections::HashMap;

/// Link reference definitions: normalized label → (raw destination, raw title).
pub type RefMap = HashMap<String, (String, Option<String>)>;

/// CommonMark ASCII punctuation — the only chars a backslash may escape.
fn is_ascii_punct(b: u8) -> bool {
    matches!(b, b'!'..=b'/' | b':'..=b'@' | b'['..=b'`' | b'{'..=b'~')
}

/// Push a single (ASCII) char, HTML-escaping the text specials.
fn push_escaped_byte(out: &mut String, b: u8) {
    match b {
        b'<' => out.push_str("&lt;"),
        b'>' => out.push_str("&gt;"),
        b'&' => out.push_str("&amp;"),
        b'"' => out.push_str("&quot;"),
        _ => out.push(b as char),
    }
}

/// Push a resolved code point, HTML-escaping the text specials. U+0000,
/// surrogates, and out-of-range all become U+FFFD (CommonMark).
fn push_char_escaped(out: &mut String, cp: u32) {
    let ch = if cp == 0 {
        '\u{FFFD}'
    } else {
        char::from_u32(cp).unwrap_or('\u{FFFD}')
    };
    match ch {
        '<' => out.push_str("&lt;"),
        '>' => out.push_str("&gt;"),
        '&' => out.push_str("&amp;"),
        '"' => out.push_str("&quot;"),
        c => out.push(c),
    }
}

/// A resolved character reference: a numeric code point, or the (possibly
/// two-character) expansion of a named entity.
enum Resolved {
    Cp(u32),
    Text(&'static str),
}

/// Append `s` to `out`, HTML-escaping the text specials.
fn push_str_escaped(out: &mut String, s: &str) {
    for c in s.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
}

/// Try to parse an entity or numeric character reference at `bytes[i]` (`&`).
/// Returns `(resolved value, bytes consumed including the `&` and `;`)`.
fn parse_entity(bytes: &[u8], i: usize) -> Option<(Resolved, usize)> {
    let rest = &bytes[i + 1..];
    if rest.first() == Some(&b'#') {
        let body = &rest[1..];
        let hex = matches!(body.first(), Some(b'x' | b'X'));
        let digits = &body[hex as usize..];
        let max = if hex { 6 } else { 7 };
        let mut value: u32 = 0;
        let mut n = 0usize;
        while n < digits.len() {
            let d = match digits[n] {
                c @ b'0'..=b'9' => c - b'0',
                c @ b'a'..=b'f' if hex => c - b'a' + 10,
                c @ b'A'..=b'F' if hex => c - b'A' + 10,
                _ => break,
            };
            value = value.saturating_mul(if hex { 16 } else { 10 }) + d as u32;
            n += 1;
            if n > max {
                return None;
            }
        }
        if n == 0 || digits.get(n) != Some(&b';') {
            return None;
        }
        Some((
            Resolved::Cp(remap_numeric(value)),
            1 + 1 + hex as usize + n + 1,
        ))
    } else {
        let mut n = 0usize;
        while n < rest.len() && rest[n].is_ascii_alphanumeric() {
            n += 1;
        }
        if n == 0 || rest.get(n) != Some(&b';') {
            return None;
        }
        let name = std::str::from_utf8(&rest[..n]).ok()?;
        Some((Resolved::Text(named(name)?), 1 + n + 1))
    }
}

/// Resolve backslash escapes and entity references in `s`, returning the raw
/// character value (used for link destinations and titles before attribute
/// escaping). Borrows when there is nothing to unescape (no `\` or `&`).
pub(crate) fn unescape_string(s: &str) -> Cow<'_, str> {
    let bytes = s.as_bytes();
    if !bytes.iter().any(|&b| b == b'\\' || b == b'&') {
        return Cow::Borrowed(s);
    }
    let mut out = String::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if i + 1 < bytes.len() && is_ascii_punct(bytes[i + 1]) => {
                out.push(bytes[i + 1] as char);
                i += 2;
            }
            b'&' => {
                if let Some((val, c)) = parse_entity(bytes, i) {
                    match val {
                        Resolved::Cp(cp) => out.push(if cp == 0 {
                            '\u{FFFD}'
                        } else {
                            char::from_u32(cp).unwrap_or('\u{FFFD}')
                        }),
                        Resolved::Text(s) => out.push_str(s),
                    }
                    i += c;
                } else {
                    out.push('&');
                    i += 1;
                }
            }
            _ => {
                let ch = s[i..].chars().next().unwrap();
                out.push(ch);
                i += ch.len_utf8();
            }
        }
    }
    Cow::Owned(out)
}

/// Is `b` safe to leave unescaped in an `href`? Mirrors cmark's HREF_SAFE set.
fn href_safe(b: u8) -> bool {
    matches!(b,
        b'!' | b'#' | b'$' | b'%' | b'\'' | b'(' | b')' | b'*' | b'+' | b',' | b'-' | b'.' | b'/'
        | b'0'..=b'9' | b':' | b';' | b'=' | b'?' | b'@'
        | b'A'..=b'Z' | b'_'
        | b'a'..=b'z' | b'~')
}

/// Append `s` to `out` escaped for an `href` attribute (percent-encode unsafe
/// bytes; `&` → `&amp;`, `'` → `&#x27;`), matching cmark's `houdini_href`.
fn escape_href(s: &str, out: &mut String) {
    for &b in s.as_bytes() {
        if b < 0x80 && href_safe(b) {
            out.push(b as char);
        } else if b == b'&' {
            out.push_str("&amp;");
        } else if b == b'\'' {
            out.push_str("&#x27;");
        } else {
            out.push('%');
            out.push(
                char::from_digit((b >> 4) as u32, 16)
                    .unwrap()
                    .to_ascii_uppercase(),
            );
            out.push(
                char::from_digit((b & 0xf) as u32, 16)
                    .unwrap()
                    .to_ascii_uppercase(),
            );
        }
    }
}

/// Append `s` to `out` escaped for a double-quoted attribute (`&<>"`).
fn escape_attr(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
}

/// Try to parse a code span opening at `bytes[i]` (a backtick). On success
/// emits `<code>…</code>` and returns the index past the closing backticks.
fn try_code_span(src: &str, bytes: &[u8], i: usize, out: &mut String) -> Option<usize> {
    let open_len = bytes[i..].iter().take_while(|&&b| b == b'`').count();
    let content_start = i + open_len;
    let mut j = content_start;
    while j < bytes.len() {
        if bytes[j] == b'`' {
            let run = bytes[j..].iter().take_while(|&&b| b == b'`').count();
            if run == open_len {
                emit_code_span(&src[content_start..j], out);
                return Some(j + run);
            }
            j += run;
        } else {
            j += 1;
        }
    }
    None
}

/// A code span is bounded by single spaces (but not all spaces)?
fn code_span_strips(s: &str) -> bool {
    s.len() >= 2 && s.starts_with(' ') && s.ends_with(' ') && s.bytes().any(|b| b != b' ')
}

/// Render a code span interior: line endings become spaces, a single space is
/// stripped from each end when bounded by spaces (but not all-spaces). The
/// common case (no embedded newline) escapes a slice directly — no allocation.
fn emit_code_span(content: &str, out: &mut String) {
    out.push_str("<code>");
    if content.as_bytes().contains(&b'\n') {
        // Rare: convert line endings to spaces, then strip surrounding space.
        let mut s: String = content
            .chars()
            .map(|c| if c == '\n' { ' ' } else { c })
            .collect();
        if code_span_strips(&s) {
            s.remove(0);
            s.pop();
        }
        escape_html(&s, out);
    } else {
        let body = if code_span_strips(content) {
            &content[1..content.len() - 1]
        } else {
            content
        };
        escape_html(body, out);
    }
    out.push_str("</code>");
}

/// Is `s` (between `<` and `>`) an absolute-URI autolink?
fn is_uri_autolink(s: &str) -> bool {
    let Some(colon) = s.find(':') else {
        return false;
    };
    let scheme = &s[..colon];
    if scheme.len() < 2
        || scheme.len() > 32
        || !scheme.as_bytes()[0].is_ascii_alphabetic()
        || !scheme
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'.' | b'-'))
    {
        return false;
    }
    s[colon + 1..].bytes().all(|b| b > 0x20 && b != b'<')
}

/// Is `s` an email autolink (a restricted form of RFC 5322 addr-spec)?
fn is_email_autolink(s: &str) -> bool {
    let Some(at) = s.find('@') else { return false };
    let (local, domain) = (&s[..at], &s[at + 1..]);
    if local.is_empty()
        || !local
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".!#$%&'*+/=?^_`{|}~-".contains(&b))
    {
        return false;
    }
    !domain.is_empty()
        && domain.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
                && label.as_bytes()[0] != b'-'
                && *label.as_bytes().last().unwrap() != b'-'
        })
}

/// Try to parse an autolink at `bytes[i]` (`<`).
fn try_autolink(src: &str, bytes: &[u8], i: usize) -> Option<(usize, String)> {
    let close = bytes[i + 1..].iter().position(|&b| b == b'>')?;
    let content = &src[i + 1..i + 1 + close];
    let consumed = close + 2;

    let mailto = if is_uri_autolink(content) {
        ""
    } else if is_email_autolink(content) {
        "mailto:"
    } else {
        return None;
    };

    let mut html = String::from("<a href=\"");
    html.push_str(mailto);
    escape_href(content, &mut html);
    html.push_str("\">");
    escape_html(content, &mut html);
    html.push_str("</a>");
    Some((consumed, html))
}

fn find_sub(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn is_html_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n')
}

/// Try to match a raw inline HTML construct (tag, comment, PI, declaration, or
/// CDATA) at `bytes[i]` (`<`), returning the bytes consumed. The match is
/// emitted verbatim by the caller.
fn try_raw_html(bytes: &[u8], i: usize) -> Option<usize> {
    let r = &bytes[i..];
    // Comment: <!-->, <!--->, or <!-- text-not-containing--> -->
    if r.starts_with(b"<!--") {
        let after = &r[4..];
        if after.first() == Some(&b'>') {
            return Some(i + 5); // <!-->
        }
        if after.starts_with(b"->") {
            return Some(i + 6); // <!--->
        }
        return find_sub(after, b"-->").map(|p| i + 4 + p + 3);
    }
    // CDATA.
    if r.starts_with(b"<![CDATA[") {
        return find_sub(&r[9..], b"]]>").map(|p| i + 9 + p + 3);
    }
    // Declaration: <! + ASCII letter ... >
    if r.starts_with(b"<!") && r.get(2).is_some_and(u8::is_ascii_alphabetic) {
        return r[2..]
            .iter()
            .position(|&c| c == b'>')
            .map(|p| i + 2 + p + 1);
    }
    // Processing instruction.
    if r.starts_with(b"<?") {
        return find_sub(&r[2..], b"?>").map(|p| i + 2 + p + 2);
    }
    // Closing tag: </name optional-ws >
    if r.starts_with(b"</") {
        let mut k = 2;
        if !r.get(k).is_some_and(u8::is_ascii_alphabetic) {
            return None;
        }
        k += 1;
        while r
            .get(k)
            .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'-')
        {
            k += 1;
        }
        while r.get(k).is_some_and(|c| is_html_ws(*c)) {
            k += 1;
        }
        return (r.get(k) == Some(&b'>')).then_some(i + k + 1);
    }
    // Open tag: <name attrs* optional-ws /?>
    if r.get(1).is_some_and(u8::is_ascii_alphabetic) {
        let mut k = 2;
        while r
            .get(k)
            .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'-')
        {
            k += 1;
        }
        loop {
            let mut ws = 0;
            while r.get(k).is_some_and(|c| is_html_ws(*c)) {
                k += 1;
                ws += 1;
            }
            if !r
                .get(k)
                .is_some_and(|c| c.is_ascii_alphabetic() || matches!(c, b'_' | b':'))
            {
                break;
            }
            if ws == 0 {
                return None; // attributes must be preceded by whitespace
            }
            k += 1;
            while r.get(k).is_some_and(|c| {
                c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b':' | b'-')
            }) {
                k += 1;
            }
            // Optional `= value`.
            let mut w = k;
            while r.get(w).is_some_and(|c| is_html_ws(*c)) {
                w += 1;
            }
            if r.get(w) == Some(&b'=') {
                w += 1;
                while r.get(w).is_some_and(|c| is_html_ws(*c)) {
                    w += 1;
                }
                match r.get(w) {
                    Some(&q @ (b'"' | b'\'')) => {
                        w += 1;
                        while r.get(w).is_some_and(|c| *c != q) {
                            w += 1;
                        }
                        if r.get(w) != Some(&q) {
                            return None;
                        }
                        w += 1;
                    }
                    Some(_) => {
                        let s = w;
                        while r.get(w).is_some_and(|c| {
                            !is_html_ws(*c)
                                && !matches!(c, b'"' | b'\'' | b'=' | b'<' | b'>' | b'`')
                        }) {
                            w += 1;
                        }
                        if w == s {
                            return None;
                        }
                    }
                    None => return None,
                }
                k = w;
            }
        }
        while r.get(k).is_some_and(|c| is_html_ws(*c)) {
            k += 1;
        }
        if r.get(k) == Some(&b'/') {
            k += 1;
        }
        return (r.get(k) == Some(&b'>')).then_some(i + k + 1);
    }
    None
}

/// Length of a complete open or closing HTML tag at `rest[0]` (`<`), used by
/// the HTML-block condition-7 matcher. Returns `None` if not a tag.
pub(crate) fn raw_tag_len(rest: &[u8]) -> Option<usize> {
    match try_raw_html(rest, 0) {
        // Only plain tags qualify (exclude comments/PI/declaration/CDATA).
        Some(end) if !rest.starts_with(b"<!") && !rest.starts_with(b"<?") => Some(end),
        _ => None,
    }
}

/// Skip spaces/tabs at `i`.
fn skip_spaces(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
        i += 1;
    }
    i
}

/// Is `s` already a normalized label (trimmed, single-spaced, lowercase ASCII)?
/// Such labels — the common case — need no rewrite.
fn already_normalized(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.first() == Some(&b' ') || bytes.last() == Some(&b' ') {
        return false;
    }
    let mut prev_space = false;
    for &b in bytes {
        match b {
            b' ' if prev_space => return false,
            b' ' => prev_space = true,
            b'A'..=b'Z' => return false,    // needs lowercasing
            0x09..=0x0d => return false,    // other whitespace needs collapsing
            0..=0x7f => prev_space = false, // plain ASCII
            _ => return false,              // non-ASCII: rebuild to case-fold safely
        }
    }
    true
}

/// Append the normalized form of `s` (trim, collapse whitespace, case-fold) to
/// `out`.
fn normalize_label_append(s: &str, out: &mut String) {
    let mut prev_ws = false;
    for c in s.trim().chars() {
        if c.is_whitespace() {
            if !prev_ws {
                out.push(' ');
                prev_ws = true;
            }
        } else {
            // Case-fold (approximated by lowercasing, plus the ß/ẞ → "ss"
            // special case the Unicode fold applies).
            for lc in c.to_lowercase() {
                if lc == 'ß' {
                    out.push_str("ss");
                } else {
                    out.push(lc);
                }
            }
            prev_ws = false;
        }
    }
}

/// A normalized lookup key for `s`: borrowed when `s` is already normalized,
/// otherwise written into the reused `buf` — no per-lookup allocation.
fn norm_key<'a>(s: &'a str, buf: &'a mut String) -> &'a str {
    if already_normalized(s) {
        s
    } else {
        buf.clear();
        normalize_label_append(s, buf);
        buf
    }
}

/// Normalize a link label: trim, collapse internal whitespace, case-fold.
/// Borrows when the label is already normalized.
fn normalize_label(s: &str) -> Cow<'_, str> {
    if already_normalized(s) {
        return Cow::Borrowed(s);
    }
    let mut out = String::new();
    normalize_label_append(s, &mut out);
    Cow::Owned(out)
}

// ---- node list -----------------------------------------------------------

/// An inline node. Phase 1 builds a doubly-linked list of these; phase 2/3
/// (emphasis, links) splice tags before phase 4 renders.
enum Node {
    /// A `[start, end)` range of computed HTML in the shared `cur` buffer
    /// (escaped text, code spans, links/images) — no per-segment allocation.
    Span { start: usize, end: usize },
    /// A run of emphasis delimiters, literal until paired.
    Delim {
        ch: u8,
        count: usize,
        orig: usize,
        can_open: bool,
        can_close: bool,
    },
    /// A static tag/literal: emphasis tags, `</a>`, or an unconsumed `[`/`![`/`]`.
    Tag(&'static str),
}

struct Slot {
    node: Node,
    prev: Option<usize>,
    next: Option<usize>,
}

/// A doubly-linked list over an append-only slot arena (slots are never freed,
/// only unlinked, so indices stay stable for the delimiter stack).
struct List {
    slots: Vec<Slot>,
    head: Option<usize>,
    tail: Option<usize>,
}

impl List {
    fn new() -> Self {
        List {
            // Pre-sized to a typical paragraph's inline-node count so the early
            // growth reallocs (and their copies) don't happen per render.
            slots: Vec::with_capacity(64),
            head: None,
            tail: None,
        }
    }

    fn push(&mut self, node: Node) -> usize {
        let idx = self.slots.len();
        self.slots.push(Slot {
            node,
            prev: self.tail,
            next: None,
        });
        match self.tail {
            Some(t) => self.slots[t].next = Some(idx),
            None => self.head = Some(idx),
        }
        self.tail = Some(idx);
        idx
    }

    fn splice_after(&mut self, at: usize, node: Node) {
        let idx = self.slots.len();
        let next = self.slots[at].next;
        self.slots.push(Slot {
            node,
            prev: Some(at),
            next,
        });
        self.slots[at].next = Some(idx);
        match next {
            Some(n) => self.slots[n].prev = Some(idx),
            None => self.tail = Some(idx),
        }
    }

    fn splice_before(&mut self, at: usize, node: Node) {
        let idx = self.slots.len();
        let prev = self.slots[at].prev;
        self.slots.push(Slot {
            node,
            prev,
            next: Some(at),
        });
        self.slots[at].prev = Some(idx);
        match prev {
            Some(p) => self.slots[p].next = Some(idx),
            None => self.head = Some(idx),
        }
    }

    fn unlink(&mut self, idx: usize) {
        let (prev, next) = (self.slots[idx].prev, self.slots[idx].next);
        match prev {
            Some(p) => self.slots[p].next = next,
            None => self.head = next,
        }
        match next {
            Some(n) => self.slots[n].prev = prev,
            None => self.tail = prev,
        }
    }
}

/// Render a single node to `out`. `cur` backs `Span` nodes.
fn render_node(node: &Node, cur: &str, out: &mut String) {
    match node {
        Node::Span { start, end } => out.push_str(&cur[*start..*end]),
        Node::Tag(t) => out.push_str(t),
        Node::Delim { ch, count, .. } => {
            for _ in 0..*count {
                out.push(*ch as char);
            }
        }
    }
}

/// Render the list to `out`, starting at `head`.
fn render_list(list: &List, cur: &str, out: &mut String) {
    let mut node = list.head;
    while let Some(idx) = node {
        render_node(&list.slots[idx].node, cur, out);
        node = list.slots[idx].next;
    }
}

// ---- delimiter stack -----------------------------------------------------

enum StackItem {
    /// Emphasis delimiter run; the data lives in the `Node::Delim` at `node`.
    Emph(usize),
    /// A `[` (or `![`) opener. `text_src` is the source offset just after it.
    Bracket {
        node: usize,
        image: bool,
        active: bool,
        text_src: usize,
    },
}

/// Is the boundary char (`None` at the edge) Unicode whitespace?
fn boundary_ws(c: Option<char>) -> bool {
    c.is_none_or(|c| c.is_whitespace())
}

/// Is the boundary char a punctuation character for flanking? ASCII
/// punctuation, or any non-ASCII char that isn't alphanumeric or whitespace
/// (a proxy for the Unicode P*/S* categories).
fn boundary_punct(c: Option<char>) -> bool {
    c.is_some_and(|c| {
        if c.is_ascii() {
            is_ascii_punct(c as u8)
        } else {
            !c.is_alphanumeric() && !c.is_whitespace()
        }
    })
}

/// Compute `(can_open, can_close)` for a delimiter run.
fn flanking(ch: u8, before: Option<char>, after: Option<char>) -> (bool, bool) {
    let (before_ws, after_ws) = (boundary_ws(before), boundary_ws(after));
    let (before_p, after_p) = (boundary_punct(before), boundary_punct(after));
    let left = !after_ws && (!after_p || before_ws || before_p);
    let right = !before_ws && (!before_p || after_ws || after_p);
    if ch == b'_' {
        (left && (!right || before_p), right && (!left || after_p))
    } else {
        (left, right)
    }
}

/// Read a delimiter node's fields.
fn delim(list: &List, idx: usize) -> Option<(u8, usize, usize, bool, bool)> {
    match list.slots[idx].node {
        Node::Delim {
            ch,
            count,
            orig,
            can_open,
            can_close,
        } => Some((ch, count, orig, can_open, can_close)),
        _ => None,
    }
}

fn set_count(list: &mut List, idx: usize, n: usize) {
    if let Node::Delim { count, .. } = &mut list.slots[idx].node {
        *count = n;
    }
}

// ---- main entry ----------------------------------------------------------

/// Reusable inline-parsing scratch — the node arena, delimiter stack, and text
/// buffer, retained across paragraphs so the common case allocates nothing.
pub struct Scratch {
    list: List,
    stack: Vec<StackItem>,
    cur: String,
    /// Reused scratch for normalized reference-link lookup keys.
    norm: String,
}

impl Scratch {
    pub fn new() -> Self {
        Scratch {
            list: List::new(),
            stack: Vec::with_capacity(16),
            cur: String::with_capacity(1024),
            norm: String::with_capacity(48),
        }
    }
    fn reset(&mut self) {
        self.list.slots.clear();
        self.list.head = None;
        self.list.tail = None;
        self.stack.clear();
        self.cur.clear();
    }
}

/// Stream inline content with no emphasis or link delimiters straight to
/// `out`: zero allocation, single pass, no node list. Mirrors the text-handling
/// arms of [`render_inline`] (kept in sync by the conformance suite).
fn stream_inline(src: &str, out: &mut String) {
    let bytes = src.as_bytes();
    let mut i = 0usize;
    let mut run = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => {
                escape_html(&src[run..i], out);
                if i + 1 < bytes.len() && bytes[i + 1] == b'\n' {
                    out.push_str("<br />\n");
                    i = skip_spaces(bytes, i + 2);
                } else if i + 1 < bytes.len() && is_ascii_punct(bytes[i + 1]) {
                    push_escaped_byte(out, bytes[i + 1]);
                    i += 2;
                } else {
                    out.push('\\');
                    i += 1;
                }
                run = i;
            }
            b'`' => {
                escape_html(&src[run..i], out);
                if let Some(new_i) = try_code_span(src, bytes, i, out) {
                    i = new_i;
                } else {
                    let n = bytes[i..].iter().take_while(|&&b| b == b'`').count();
                    out.push_str(&src[i..i + n]);
                    i += n;
                }
                run = i;
            }
            b'&' => {
                escape_html(&src[run..i], out);
                if let Some((val, consumed)) = parse_entity(bytes, i) {
                    match val {
                        Resolved::Cp(cp) => push_char_escaped(out, cp),
                        Resolved::Text(s) => push_str_escaped(out, s),
                    }
                    i += consumed;
                } else {
                    out.push_str("&amp;");
                    i += 1;
                }
                run = i;
            }
            b'<' => {
                escape_html(&src[run..i], out);
                if let Some((consumed, html)) = try_autolink(src, bytes, i) {
                    out.push_str(&html);
                    i += consumed;
                } else if let Some(end) = try_raw_html(bytes, i) {
                    out.push_str(&src[i..end]);
                    i = end;
                } else {
                    out.push_str("&lt;");
                    i += 1;
                }
                run = i;
            }
            b'\n' => {
                let line = &src[run..i];
                let trimmed = line.trim_end_matches(' ');
                let hard = line.len() - trimmed.len() >= 2;
                escape_html(trimmed, out);
                out.push_str(if hard { "<br />\n" } else { "\n" });
                i = skip_spaces(bytes, i + 1);
                run = i;
            }
            // Skip plain text to the next significant byte in one SIMD pass.
            _ => i += 1 + find_stream(&bytes[i + 1..]).unwrap_or(bytes.len() - i - 1),
        }
    }
    escape_html(src[run..].trim_end_matches(' '), out);
}

/// Parse `src` (a block's raw inline text) to HTML, appending to `out`.
#[allow(unused_assignments)] // `seg` is updated at segment ends; the last is unused
pub fn render_inline(src: &str, out: &mut String, refmap: &RefMap, scratch: &mut Scratch) {
    let bytes = src.as_bytes();
    // Fast path: no emphasis/link delimiters → stream directly, no allocation.
    if find_emph(bytes).is_none() {
        stream_inline(src, out);
        return;
    }
    scratch.reset();
    let list = &mut scratch.list;
    let stack = &mut scratch.stack;
    let cur = &mut scratch.cur;
    let norm = &mut scratch.norm;
    let mut i = 0usize;
    let mut run = 0usize;
    let mut seg = 0usize; // start (in `cur`) of the open text segment

    // Close the open text segment into a Span node.
    macro_rules! flush {
        () => {
            if cur.len() > seg {
                list.push(Node::Span {
                    start: seg,
                    end: cur.len(),
                });
                seg = cur.len();
            }
        };
    }

    while i < bytes.len() {
        match bytes[i] {
            b'\\' => {
                escape_html(&src[run..i], cur);
                if i + 1 < bytes.len() && bytes[i + 1] == b'\n' {
                    cur.push_str("<br />\n");
                    i = skip_spaces(bytes, i + 2);
                } else if i + 1 < bytes.len() && is_ascii_punct(bytes[i + 1]) {
                    push_escaped_byte(cur, bytes[i + 1]);
                    i += 2;
                } else {
                    cur.push('\\');
                    i += 1;
                }
                run = i;
            }
            b'`' => {
                escape_html(&src[run..i], cur);
                if let Some(new_i) = try_code_span(src, bytes, i, cur) {
                    i = new_i;
                } else {
                    let n = bytes[i..].iter().take_while(|&&b| b == b'`').count();
                    cur.push_str(&src[i..i + n]);
                    i += n;
                }
                run = i;
            }
            b'&' => {
                escape_html(&src[run..i], cur);
                if let Some((val, consumed)) = parse_entity(bytes, i) {
                    match val {
                        Resolved::Cp(cp) => push_char_escaped(cur, cp),
                        Resolved::Text(s) => push_str_escaped(cur, s),
                    }
                    i += consumed;
                } else {
                    cur.push_str("&amp;");
                    i += 1;
                }
                run = i;
            }
            b'<' => {
                escape_html(&src[run..i], cur);
                if let Some((consumed, html)) = try_autolink(src, bytes, i) {
                    cur.push_str(&html);
                    i += consumed;
                } else if let Some(end) = try_raw_html(bytes, i) {
                    cur.push_str(&src[i..end]); // verbatim
                    i = end;
                } else {
                    cur.push_str("&lt;");
                    i += 1;
                }
                run = i;
            }
            b'*' | b'_' => {
                let ch = bytes[i];
                escape_html(&src[run..i], cur);
                flush!();
                let count = bytes[i..].iter().take_while(|&&b| b == ch).count();
                let before = src[..i].chars().next_back();
                let after = src[i + count..].chars().next();
                let (can_open, can_close) = flanking(ch, before, after);
                let idx = list.push(Node::Delim {
                    ch,
                    count,
                    orig: count,
                    can_open,
                    can_close,
                });
                stack.push(StackItem::Emph(idx));
                i += count;
                run = i;
            }
            b'[' => {
                escape_html(&src[run..i], cur);
                flush!();
                let node = list.push(Node::Tag("["));
                stack.push(StackItem::Bracket {
                    node,
                    image: false,
                    active: true,
                    text_src: i + 1,
                });
                i += 1;
                run = i;
            }
            b'!' if bytes.get(i + 1) == Some(&b'[') => {
                escape_html(&src[run..i], cur);
                flush!();
                let node = list.push(Node::Tag("!["));
                stack.push(StackItem::Bracket {
                    node,
                    image: true,
                    active: true,
                    text_src: i + 2,
                });
                i += 2;
                run = i;
            }
            b']' => {
                escape_html(&src[run..i], cur);
                flush!();
                let rb = list.push(Node::Tag("]"));
                let rb_src = i;
                i += 1;
                look_for_link_or_image(
                    src, bytes, &mut i, list, stack, cur, norm, refmap, rb, rb_src,
                );
                // A resolved link/image appended its tag to `cur` and spanned it
                // directly; the next text segment starts after it.
                seg = cur.len();
                run = i;
            }
            b'\n' => {
                // Trailing spaces in the pending run decide the break kind.
                let line = &src[run..i];
                let trimmed = line.trim_end_matches(' ');
                let hard = line.len() - trimmed.len() >= 2;
                escape_html(trimmed, cur);
                cur.push_str(if hard { "<br />\n" } else { "\n" });
                i = skip_spaces(bytes, i + 1);
                run = i;
            }
            // Skip plain text to the next significant byte in one SIMD pass.
            _ => i += 1 + find_inline(&bytes[i + 1..]).unwrap_or(bytes.len() - i - 1),
        }
    }
    // Trailing spaces at the very end of a block are dropped (no following line
    // to form a hard break).
    escape_html(src[run..].trim_end_matches(' '), cur);
    flush!();

    process_emphasis(list, stack, 0);
    render_list(list, cur, out);
}

/// CommonMark "look for link or image": on `]`, find the matching opener and,
/// if the following syntax forms a valid inline or reference link, wrap the
/// enclosed nodes. `i` already points past the `]`.
#[allow(clippy::too_many_arguments)]
fn look_for_link_or_image(
    src: &str,
    bytes: &[u8],
    i: &mut usize,
    list: &mut List,
    stack: &mut Vec<StackItem>,
    cur: &mut String,
    norm: &mut String,
    refmap: &RefMap,
    rb_node: usize,
    rb_src: usize,
) {
    let Some(op) = stack
        .iter()
        .rposition(|e| matches!(e, StackItem::Bracket { .. }))
    else {
        return; // no opener: ] stays literal
    };
    let (op_node, image, active, text_src) = match stack[op] {
        StackItem::Bracket {
            node,
            image,
            active,
            text_src,
        } => (node, image, active, text_src),
        _ => unreachable!(),
    };
    if !active {
        stack.remove(op);
        return;
    }

    let text = &src[text_src..rb_src];
    let Some((dest_raw, title_raw, new_i)) = parse_link_target(src, bytes, *i, refmap, text, norm)
    else {
        stack.remove(op);
        return; // ] stays literal
    };

    // Resolve emphasis within the link text (bounded below by the opener).
    process_emphasis(list, stack, op + 1);

    if image {
        // Alt text = the rendered link text with tags stripped.
        let mut inner = String::new();
        let mut node = list.slots[op_node].next;
        while let Some(idx) = node {
            if idx == rb_node {
                break;
            }
            render_node(&list.slots[idx].node, cur, &mut inner);
            node = list.slots[idx].next;
        }
        let alt = strip_tags(&inner);

        // Build the <img> tag into the shared buffer (no per-image allocation).
        let start = cur.len();
        cur.push_str("<img src=\"");
        escape_href(unescape_string(dest_raw).as_ref(), cur);
        cur.push_str("\" alt=\"");
        cur.push_str(&alt);
        cur.push('"');
        if let Some(t) = title_raw {
            cur.push_str(" title=\"");
            escape_attr(unescape_string(t).as_ref(), cur);
            cur.push('"');
        }
        cur.push_str(" />");
        let end = cur.len();
        list.slots[op_node].node = Node::Span { start, end };

        // Unlink the link text and the closing bracket.
        let mut c = list.slots[op_node].next;
        while let Some(n) = c {
            let nxt = list.slots[n].next;
            list.unlink(n);
            if n == rb_node {
                break;
            }
            c = nxt;
        }
    } else {
        // Build the <a> open tag into the shared buffer (no per-link allocation).
        let start = cur.len();
        cur.push_str("<a href=\"");
        escape_href(unescape_string(dest_raw).as_ref(), cur);
        cur.push('"');
        if let Some(t) = title_raw {
            cur.push_str(" title=\"");
            escape_attr(unescape_string(t).as_ref(), cur);
            cur.push('"');
        }
        cur.push('>');
        let end = cur.len();
        list.slots[op_node].node = Node::Span { start, end };
        list.slots[rb_node].node = Node::Tag("</a>");

        // No links inside links: deactivate earlier `[` openers.
        for e in stack.iter_mut() {
            if let StackItem::Bracket {
                image: false,
                active,
                ..
            } = e
            {
                *active = false;
            }
        }
    }

    stack.truncate(op); // drop the opener and any delimiters above it
    *i = new_i;
}

/// Remove `<...>` tag spans from `s` to derive image alt text. A nested
/// `<img>` contributes its own `alt` attribute (CommonMark's plain-text rule).
fn strip_tags(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    while i < s.len() {
        if bytes[i] == b'<' {
            let end = s[i..].find('>').map_or(s.len(), |p| i + p + 1);
            let tag = &s[i..end];
            if let Some(rest) = tag.strip_prefix("<img")
                && let Some(ap) = rest.find("alt=\"")
            {
                let after = &rest[ap + 5..];
                if let Some(q) = after.find('"') {
                    out.push_str(&after[..q]);
                }
            }
            i = end;
        } else {
            let ch = s[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

/// Parse the link target after a `]` at `i`: inline `(dest "title")`, or a
/// full/collapsed/shortcut reference resolved against `refmap`. `text` is the
/// raw link-text source (the shortcut/collapsed label). Returns
/// `(raw dest, raw title, index past the target)`.
fn parse_link_target<'a>(
    src: &'a str,
    bytes: &[u8],
    i: usize,
    refmap: &'a RefMap,
    text: &'a str,
    norm: &mut String,
) -> Option<(&'a str, Option<&'a str>, usize)> {
    if bytes.get(i) == Some(&b'(')
        && let Some(r) = parse_inline_paren(src, bytes, i)
    {
        return Some(r);
    }
    // Full reference: [label]
    if bytes.get(i) == Some(&b'[') {
        if let Some((label, end)) = read_bracket_label(src, bytes, i) {
            // collapsed [] uses the link text
            let label = if label.trim().is_empty() { text } else { label };
            let (d, t) = refmap.get(norm_key(label, norm))?;
            return Some((d.as_str(), t.as_deref(), end));
        }
        return None;
    }
    // Shortcut reference: the link text itself is the label.
    let (d, t) = refmap.get(norm_key(text, norm))?;
    Some((d.as_str(), t.as_deref(), i))
}

/// Read a `[label]` starting at `bytes[i]` (`[`). Returns `(label, index past
/// the `]`)`. Nested unescaped brackets are disallowed.
fn read_bracket_label<'a>(src: &'a str, bytes: &[u8], i: usize) -> Option<(&'a str, usize)> {
    let mut j = i + 1;
    while j < bytes.len() {
        match bytes[j] {
            b'\\' if j + 1 < bytes.len() => j += 2,
            b']' => return Some((&src[i + 1..j], j + 1)),
            b'[' => return None,
            _ => j += 1,
        }
        if j - i > 1000 {
            return None;
        }
    }
    None
}

/// Parse an inline link tail `(dest "title")` starting at `bytes[i]` (`(`).
fn parse_inline_paren<'a>(
    src: &'a str,
    bytes: &[u8],
    i: usize,
) -> Option<(&'a str, Option<&'a str>, usize)> {
    let mut j = skip_ws(bytes, i + 1);
    let (dest, dj, _) = parse_dest(src, bytes, j)?;
    let before = dj;
    j = skip_ws(bytes, dj);
    let title = if j > before {
        match parse_title(src, bytes, j) {
            Some((t, tj)) => {
                j = tj;
                Some(t)
            }
            None => {
                j = before;
                None
            }
        }
    } else {
        j = before;
        None
    };
    j = skip_ws(bytes, j);
    if bytes.get(j) != Some(&b')') {
        return None;
    }
    Some((dest, title, j + 1))
}

/// Skip spaces, tabs, and newlines (the caller guards against blank lines).
fn skip_ws(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && matches!(bytes[i], b' ' | b'\t' | b'\n') {
        i += 1;
    }
    i
}

/// Parse a link destination at `bytes[j]`. Returns `(raw dest, end index, was
/// angle-bracketed)`. Empty bare destinations are allowed (valid inline, but
/// rejected by the ref-def caller).
fn parse_dest<'a>(text: &'a str, bytes: &[u8], j: usize) -> Option<(&'a str, usize, bool)> {
    if bytes.get(j) == Some(&b'<') {
        let s = j + 1;
        let mut k = s;
        loop {
            match bytes.get(k)? {
                b'\n' | b'<' => return None,
                b'\\' if k + 1 < bytes.len() => k += 2,
                b'>' => break,
                _ => k += 1,
            }
        }
        Some((&text[s..k], k + 1, true))
    } else {
        let s = j;
        let mut k = j;
        let mut depth = 0i32;
        while k < bytes.len() {
            match bytes[k] {
                b'\\' if k + 1 < bytes.len() => k += 2,
                b'(' => {
                    depth += 1;
                    k += 1;
                }
                b')' => {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                    k += 1;
                }
                b if b == b' ' || b == b'\t' || b == b'\n' || b < 0x20 => break,
                _ => k += 1,
            }
        }
        if depth != 0 {
            return None;
        }
        Some((&text[s..k], k, false))
    }
}

/// Parse a link title at `bytes[j]` (`"`, `'`, or `(`). Returns `(raw title,
/// end index)`.
fn parse_title<'a>(text: &'a str, bytes: &[u8], j: usize) -> Option<(&'a str, usize)> {
    let q = *bytes.get(j)?;
    if q != b'"' && q != b'\'' && q != b'(' {
        return None;
    }
    let close = if q == b'(' { b')' } else { q };
    let s = j + 1;
    let mut k = s;
    loop {
        match bytes.get(k)? {
            b'\\' if k + 1 < bytes.len() => k += 2,
            b'(' if q == b'(' => return None,
            b if *b == close => break,
            _ => k += 1,
        }
    }
    Some((&text[s..k], k + 1))
}

/// Extract leading link reference definitions from a paragraph's text. Returns
/// the offset where the remaining paragraph begins and the defs as
/// `(normalized label, raw destination, raw title)`.
pub fn take_ref_defs(text: &str) -> (usize, Vec<(String, String, Option<String>)>) {
    let bytes = text.as_bytes();
    let mut pos = 0;
    let mut defs = Vec::new();
    while let Some((end, label, dest, title)) = parse_ref_def(text, bytes, pos) {
        defs.push((normalize_label(&label).into_owned(), dest, title));
        pos = end;
    }
    (pos, defs)
}

fn parse_ref_def(
    text: &str,
    bytes: &[u8],
    start: usize,
) -> Option<(usize, String, String, Option<String>)> {
    let mut j = start;
    let mut ind = 0;
    while ind < 3 && bytes.get(j) == Some(&b' ') {
        j += 1;
        ind += 1;
    }
    if bytes.get(j) != Some(&b'[') {
        return None;
    }
    let (label, after) = read_bracket_label(text, bytes, j)?;
    if bytes.get(after) != Some(&b':') || normalize_label(label).is_empty() {
        return None;
    }
    j = ref_spnl(bytes, after + 1);
    let (dest, dj, angle) = parse_dest(text, bytes, j)?;
    if dest.is_empty() && !angle {
        return None;
    }
    let jt = ref_spnl(bytes, dj);
    let (title, after_title) = match parse_title(text, bytes, jt) {
        Some((t, tj)) if jt > dj => (Some(t), tj),
        _ => (None, dj),
    };
    if let Some(end) = ref_line_end(bytes, after_title) {
        // The RefMap owns its entries (it outlives the borrowed source).
        return Some((
            end,
            label.to_string(),
            dest.to_string(),
            title.map(String::from),
        ));
    }
    // A trailing-junk title invalidates only the title, not the whole def.
    if title.is_some()
        && let Some(end) = ref_line_end(bytes, dj)
    {
        return Some((end, label.to_string(), dest.to_string(), None));
    }
    None
}

/// Skip spaces/tabs and at most one line ending (then more spaces/tabs).
fn ref_spnl(bytes: &[u8], j: usize) -> usize {
    let j = skip_spaces(bytes, j);
    if bytes.get(j) == Some(&b'\n') {
        skip_spaces(bytes, j + 1)
    } else {
        j
    }
}

/// If the rest of the line at `j` is blank, return the index past its newline.
fn ref_line_end(bytes: &[u8], j: usize) -> Option<usize> {
    let k = skip_spaces(bytes, j);
    match bytes.get(k) {
        None => Some(k),
        Some(&b'\n') => Some(k + 1),
        _ => None,
    }
}

/// Phase 2: pair emphasis delimiters on the stack at or above `start`,
/// splicing `<em>`/`<strong>` tags into the list. Removes consumed entries.
fn process_emphasis(list: &mut List, stack: &mut Vec<StackItem>, start: usize) {
    let mut openers_bottom = [[-1isize; 3]; 2];
    let mut ci = start;

    while ci < stack.len() {
        let StackItem::Emph(cnode) = stack[ci] else {
            ci += 1;
            continue;
        };
        let Some((cch, ccount, corig, ccan_open, ccan_close)) = delim(list, cnode) else {
            ci += 1;
            continue;
        };
        if !ccan_close || ccount == 0 {
            ci += 1;
            continue;
        }
        let char_idx = if cch == b'*' { 0 } else { 1 };
        let bottom = openers_bottom[char_idx][corig % 3];

        let mut opener: Option<usize> = None;
        let mut oi = ci;
        while oi > start {
            oi -= 1;
            let StackItem::Emph(onode) = stack[oi] else {
                continue;
            };
            if (onode as isize) <= bottom {
                break;
            }
            let Some((och, ocount, oorig, ocan_open, ocan_close)) = delim(list, onode) else {
                continue;
            };
            if ocount == 0 {
                continue;
            }
            let odd_match = (ccan_open || ocan_close) && corig % 3 != 0 && (oorig + corig) % 3 == 0;
            if och == cch && ocan_open && !odd_match {
                opener = Some(oi);
                break;
            }
        }

        match opener {
            Some(oi) => {
                let StackItem::Emph(onode) = stack[oi] else {
                    unreachable!()
                };
                let ocount = delim(list, onode).unwrap().1;
                let strong = ocount >= 2 && ccount >= 2;
                let use_delims = if strong { 2 } else { 1 };
                let (open_tag, close_tag) = if strong {
                    ("<strong>", "</strong>")
                } else {
                    ("<em>", "</em>")
                };
                list.splice_after(onode, Node::Tag(open_tag));
                list.splice_before(cnode, Node::Tag(close_tag));
                set_count(list, onode, ocount - use_delims);
                set_count(list, cnode, ccount - use_delims);

                // Drop the now-enclosed delimiters between opener and closer.
                stack.drain(oi + 1..ci);
                ci = oi + 1;

                if delim(list, onode).unwrap().1 == 0 {
                    list.unlink(onode);
                    stack.remove(oi);
                    ci -= 1;
                }
                if delim(list, cnode).unwrap().1 == 0 {
                    list.unlink(cnode);
                    stack.remove(ci);
                }
            }
            None => {
                openers_bottom[char_idx][corig % 3] = if ci == 0 {
                    -1
                } else {
                    stack_node(&stack[ci - 1])
                };
                if !ccan_open {
                    stack.remove(ci);
                } else {
                    ci += 1;
                }
            }
        }
    }
}

/// Node index backing a stack entry (for openers_bottom comparison).
fn stack_node(e: &StackItem) -> isize {
    match e {
        StackItem::Emph(n) => *n as isize,
        StackItem::Bracket { node, .. } => *node as isize,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inline(s: &str) -> String {
        let mut out = String::new();
        render_inline(s, &mut out, &RefMap::new(), &mut Scratch::new());
        out
    }

    #[test]
    fn backslash_escapes_punctuation() {
        assert_eq!(inline(r"\*not emph\*"), "*not emph*");
        assert_eq!(inline(r"\<"), "&lt;");
        assert_eq!(inline(r"\foo"), r"\foo");
    }

    #[test]
    fn still_escapes_html_specials() {
        assert_eq!(inline("a < b & c"), "a &lt; b &amp; c");
    }

    #[test]
    fn entity_and_numeric_references() {
        assert_eq!(inline("&copy;"), "©");
        assert_eq!(inline("&amp;"), "&amp;");
        assert_eq!(inline("&#42;"), "*");
        assert_eq!(inline("&#x2A;"), "*");
        assert_eq!(inline("&#0;"), "\u{FFFD}");
        assert_eq!(inline("&unknown;"), "&amp;unknown;");
    }

    #[test]
    fn code_spans() {
        assert_eq!(inline("`foo`"), "<code>foo</code>");
        assert_eq!(inline("``a`b``"), "<code>a`b</code>");
        assert_eq!(inline("`<&>`"), "<code>&lt;&amp;&gt;</code>");
    }

    #[test]
    fn emphasis_and_strong() {
        assert_eq!(inline("*foo*"), "<em>foo</em>");
        assert_eq!(inline("**foo**"), "<strong>foo</strong>");
        assert_eq!(inline("***foo***"), "<em><strong>foo</strong></em>");
        assert_eq!(inline("foo_bar_baz"), "foo_bar_baz");
        assert_eq!(
            inline("*foo **bar** baz*"),
            "<em>foo <strong>bar</strong> baz</em>"
        );
    }

    #[test]
    fn autolinks() {
        assert_eq!(
            inline("<http://foo.bar/?q=a&b>"),
            "<a href=\"http://foo.bar/?q=a&amp;b\">http://foo.bar/?q=a&amp;b</a>"
        );
        assert_eq!(
            inline("<foo@bar.example.com>"),
            "<a href=\"mailto:foo@bar.example.com\">foo@bar.example.com</a>"
        );
    }

    #[test]
    fn raw_inline_html() {
        assert_eq!(inline("a <b>c</b> d"), "a <b>c</b> d");
        assert_eq!(inline("<a href=\"x\">"), "<a href=\"x\">");
        assert_eq!(inline("<!-- comment -->"), "<!-- comment -->");
        assert_eq!(inline("<br/>"), "<br/>");
        // not valid HTML -> escaped
        assert_eq!(inline("a < b"), "a &lt; b");
    }

    #[test]
    fn inline_links_and_images() {
        assert_eq!(inline("[link](/uri)"), "<a href=\"/uri\">link</a>");
        assert_eq!(
            inline("[link](/uri \"title\")"),
            "<a href=\"/uri\" title=\"title\">link</a>"
        );
        assert_eq!(inline("[a *b*](/u)"), "<a href=\"/u\">a <em>b</em></a>");
        assert_eq!(
            inline("![alt](/img.png)"),
            "<img src=\"/img.png\" alt=\"alt\" />"
        );
        assert_eq!(inline("![a *b*](/i)"), "<img src=\"/i\" alt=\"a b\" />");
        // no closer -> literal
        assert_eq!(inline("[not a link"), "[not a link");
        // no matching paren / def -> literal brackets
        assert_eq!(inline("[foo] bar"), "[foo] bar");
    }

    #[test]
    fn reference_links_resolve_via_map() {
        let mut map = RefMap::new();
        map.insert(
            "foo".to_string(),
            ("/url".to_string(), Some("t".to_string())),
        );
        let mut sc = Scratch::new();
        let mut out = String::new();
        render_inline("[foo]", &mut out, &map, &mut sc);
        assert_eq!(out, "<a href=\"/url\" title=\"t\">foo</a>");

        let mut out2 = String::new();
        render_inline("[bar][foo]", &mut out2, &map, &mut sc);
        assert_eq!(out2, "<a href=\"/url\" title=\"t\">bar</a>");
    }
}
