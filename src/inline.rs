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
use crate::options::Options;
use crate::render::escape_html;
use crate::scan::{
    find_emph, find_emph_gfm, find_inline, find_inline_al, find_inline_gfm, find_inline_gfm_al,
    find_stream, find_stream_al, memchr1,
};
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

// SPIKE (`ast`): text-span emitters for the wire/AST path. In `raw` mode the
// inline tokenizer writes the *un-escaped* text into the span buffer `cur`, so
// the wire reader (`emit_inline`/`list_to_tokens`/`ast_image_alt`) can borrow it
// directly with no HTML un-escape round-trip. In HTML mode (`raw == false`)
// these are byte-for-byte the original escaping helpers, so HTML output is
// unchanged. Each is the exact inverse of the former HTML un-escape, so the raw
// bytes written equal what that un-escape used to recover.

/// Text run: HTML-escape (`raw == false`) or write verbatim (`raw == true`).
#[inline]
fn emit_text(raw: bool, s: &str, out: &mut String) {
    if raw {
        out.push_str(s);
    } else {
        escape_html(s, out);
    }
}

/// Backslash-escaped ASCII byte: escape the text specials, or push it verbatim.
#[inline]
fn emit_escaped_byte(raw: bool, out: &mut String, b: u8) {
    if raw {
        out.push(b as char);
    } else {
        push_escaped_byte(out, b);
    }
}

/// Resolved code point: escape the text specials, or push it verbatim (keeping
/// the same U+FFFD remapping for U+0000/surrogates/out-of-range).
#[inline]
fn emit_char_escaped(raw: bool, out: &mut String, cp: u32) {
    if raw {
        let ch = if cp == 0 {
            '\u{FFFD}'
        } else {
            char::from_u32(cp).unwrap_or('\u{FFFD}')
        };
        out.push(ch);
    } else {
        push_char_escaped(out, cp);
    }
}

/// Resolved entity expansion text: escape the text specials, or push verbatim.
#[inline]
fn emit_str_escaped(raw: bool, out: &mut String, s: &str) {
    if raw {
        out.push_str(s);
    } else {
        push_str_escaped(out, s);
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

/// SPIKE: code-span value (mdast `inlineCode.value`) — the raw, un-escaped
/// interior as the mdast `inlineCode.value`. Unlike the HTML render path (which
/// converts line endings to spaces), mdast keeps line endings **literal**; only
/// the CommonMark code-span padding applies: strip one leading and one trailing
/// space-or-line-ending when both ends are padded and the content is not entirely
/// whitespace. Matches `mdast-util-from-markdown`. Returns the value + new index.
/// Unconditional (dead without `ast`) so AST branches compile in every build.
#[allow(dead_code)]
fn code_span_value(src: &str, bytes: &[u8], i: usize) -> Option<(String, usize)> {
    let open_len = bytes[i..].iter().take_while(|&&b| b == b'`').count();
    let content_start = i + open_len;
    let mut j = content_start;
    while j < bytes.len() {
        if bytes[j] == b'`' {
            let run = bytes[j..].iter().take_while(|&&b| b == b'`').count();
            if run == open_len {
                let content = &src[content_start..j];
                let b = content.as_bytes();
                let pad = |c: u8| matches!(c, b' ' | b'\n' | b'\r');
                let value = if b.len() >= 2
                    && pad(b[0])
                    && pad(b[b.len() - 1])
                    && b.iter().any(|&c| !pad(c))
                {
                    content[1..content.len() - 1].to_owned()
                } else {
                    content.to_owned()
                };
                return Some((value, j + run));
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
pub(crate) fn normalize_label(s: &str) -> Cow<'_, str> {
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
    /// SPIKE (AST mode only): a semantic node indexing the `sem` side-table.
    /// Dead in the HTML path (never set), so it is unreachable in `render_node`.
    Sem(u32),
    /// SPIKE (AST mode only): close of an mdast `link`.
    LinkClose,
}

/// SPIKE: semantic payload for inline constructs that are *lossy* once rendered
/// to HTML — captured at parse time (where the raw values still exist) and
/// referenced by [`Node::Sem`]. Defined unconditionally (dead without `ast`) so
/// it can flow through `look_for_link_or_image`'s signature without cfg-gating.
#[derive(Debug, Clone)]
enum Sem {
    /// Inline code: the raw (un-escaped) value.
    Code(String),
    /// `[text](url "title")` opener. `url`/`title` are the resolved destination
    /// (NOT the percent-encoded `href` — that is unrecoverable from the output).
    LinkOpen { url: String, title: Option<String> },
    /// `![alt](url "title")` — a leaf in mdast (alt is plain text).
    Image {
        url: String,
        title: Option<String>,
        alt: String,
    },
    /// `<url>` / `<email>` autolink → an mdast `link` with one text child.
    Autolink { url: String, text: String },
    /// Raw inline HTML, verbatim.
    Html(String),
    /// A hard line break.
    Break,
    /// `[text][label]` etc. → mdast `linkReference` opener (children = link text,
    /// closed by [`Node::LinkClose`]). Carries the normalized `identifier`, the
    /// raw `label`, and `reftype` (`"shortcut"`/`"collapsed"`/`"full"`).
    LinkRef {
        identifier: String,
        label: String,
        reftype: &'static str,
    },
    /// `![alt][label]` etc. → mdast `imageReference` (a leaf; alt is plain text).
    ImageRef {
        identifier: String,
        label: String,
        reftype: &'static str,
        alt: String,
    },
    // NOTE: unlike the block `Kind` variants (kept compiled — removing them
    // perturbs the hot block matches, see block.rs), the inline extension variants
    // below ARE cfg-removed. Measured: removing them does NOT perturb the
    // monomorphized `render_inline_impl` (inline/colon-heavy corpora 0% on the
    // default path) and it keeps the build leaner — so each path uses the policy
    // that is measured-optimal for it, not a single uniform one.
    /// GFM `[^label]` → mdast `footnoteReference` (a leaf).
    #[cfg(feature = "footnotes")]
    FootnoteRef { identifier: String, label: String },
    /// remark-directive inline `:name[label]{attrs}` → mdast `textDirective`. The
    /// `label` is a content byte range (re-tokenized into children at build time).
    #[cfg(feature = "directives")]
    TextDirective {
        name: String,
        attrs: Vec<(String, String)>,
        label: Option<(u32, u32)>,
    },
}

/// SPIKE: reference-resolution metadata produced by [`parse_link_target`] in AST
/// mode, so the emitter can build a `linkReference`/`imageReference` node instead
/// of a resolved `link`/`image`. `None` for inline `(url)` targets.
struct RefInfo {
    identifier: String,
    label: String,
    reftype: &'static str,
}

/// SPIKE (`ast` feature): a flat, ordered stream of semantic inline tokens —
/// emphasis/link nesting is expressed by `Open`/`Close` pairs (reconstructed
/// into a tree by `ast.rs`). This is the lossless counterpart of the HTML the
/// fast path would emit.
#[cfg(feature = "ast")]
#[derive(Debug, Clone)]
pub enum InlineTok {
    /// mdast `text` value (HTML-unescaped).
    Text(String),
    /// Open a container: `"emphasis"` | `"strong"` | `"delete"`.
    Open(&'static str),
    /// Close the matching container.
    Close(&'static str),
    /// Open an mdast `link`.
    LinkOpen { url: String, title: Option<String> },
    /// Close the matching `link`.
    LinkClose,
    /// mdast `image` (leaf).
    Image {
        url: String,
        title: Option<String>,
        alt: String,
    },
    /// mdast `inlineCode`.
    Code(String),
    /// mdast `link` from an autolink (single text child).
    Autolink { url: String, text: String },
    /// mdast `html` (raw inline).
    Html(String),
    /// mdast `break` (hard line break).
    Break,
    /// Open an mdast `linkReference` (closed by the matching [`LinkClose`]).
    LinkRefOpen {
        identifier: String,
        label: String,
        reftype: &'static str,
    },
    /// mdast `imageReference` (leaf).
    ImageRef {
        identifier: String,
        label: String,
        reftype: &'static str,
        alt: String,
    },
    /// mdast `footnoteReference` (leaf).
    #[cfg(feature = "footnotes")]
    FootnoteRef { identifier: String, label: String },
    /// mdast `textDirective`. `label` is a content byte range whose inline content
    /// becomes the directive's children (re-tokenized by the AST builder).
    #[cfg(feature = "directives")]
    TextDirective {
        name: String,
        attrs: Vec<(String, String)>,
        label: Option<(u32, u32)>,
    },
}

/// SPIKE (`ast`): an [`InlineTok`] tagged with the content byte range
/// `[start, end)` that produced it (for unist `position`). `start == u32::MAX`
/// means the span is unknown (the consumer falls back to the block span).
#[cfg(feature = "ast")]
pub struct SpanTok {
    pub tok: InlineTok,
    pub start: u32,
    pub end: u32,
}

/// SPIKE: convert the resolved slot list into the semantic [`InlineTok`] stream.
/// Spans now hold *only* plain escaped text (every lossy construct is a
/// [`Node::Sem`]/[`Node::LinkClose`]), so reconstruction is exact.
#[cfg(feature = "ast")]
fn list_to_tokens(list: &List, cur: &str, sem: &[Sem], out: &mut Vec<SpanTok>) {
    let mut node = list.head;
    while let Some(idx) = node {
        let (s, e) = list.slots[idx].cspan;
        let mut push = |tok| {
            out.push(SpanTok {
                tok,
                start: s,
                end: e,
            })
        };
        match &list.slots[idx].node {
            Node::Span { start, end } => {
                // `cur` already holds raw (un-escaped) span text in wire mode, so
                // borrow it directly — no HTML un-escape round-trip.
                let t = &cur[*start..*end];
                if !t.is_empty() {
                    push(InlineTok::Text(t.to_owned()));
                }
            }
            Node::Tag(t) => push(match *t {
                "<em>" => InlineTok::Open("emphasis"),
                "</em>" => InlineTok::Close("emphasis"),
                "<strong>" => InlineTok::Open("strong"),
                "</strong>" => InlineTok::Close("strong"),
                "<del>" => InlineTok::Open("delete"),
                "</del>" => InlineTok::Close("delete"),
                // Unconsumed literal brackets stay as text.
                lit => InlineTok::Text(lit.to_owned()),
            }),
            Node::Delim { ch, count, .. } => push(InlineTok::Text(
                core::iter::repeat_n(*ch as char, *count).collect(),
            )),
            Node::Sem(i) => push(match &sem[*i as usize] {
                Sem::Code(v) => InlineTok::Code(v.clone()),
                Sem::LinkOpen { url, title } => InlineTok::LinkOpen {
                    url: url.clone(),
                    title: title.clone(),
                },
                Sem::Image { url, title, alt } => InlineTok::Image {
                    url: url.clone(),
                    title: title.clone(),
                    alt: alt.clone(),
                },
                Sem::Autolink { url, text } => InlineTok::Autolink {
                    url: url.clone(),
                    text: text.clone(),
                },
                Sem::Html(h) => InlineTok::Html(h.clone()),
                Sem::Break => InlineTok::Break,
                Sem::LinkRef {
                    identifier,
                    label,
                    reftype,
                } => InlineTok::LinkRefOpen {
                    identifier: identifier.clone(),
                    label: label.clone(),
                    reftype,
                },
                Sem::ImageRef {
                    identifier,
                    label,
                    reftype,
                    alt,
                } => InlineTok::ImageRef {
                    identifier: identifier.clone(),
                    label: label.clone(),
                    reftype,
                    alt: alt.clone(),
                },
                #[cfg(feature = "footnotes")]
                Sem::FootnoteRef { identifier, label } => InlineTok::FootnoteRef {
                    identifier: identifier.clone(),
                    label: label.clone(),
                },
                #[cfg(feature = "directives")]
                Sem::TextDirective { name, attrs, label } => InlineTok::TextDirective {
                    name: name.clone(),
                    attrs: attrs.clone(),
                    label: *label,
                },
            }),
            Node::LinkClose => push(InlineTok::LinkClose),
        }
        node = list.slots[idx].next;
    }
}

/// SPIKE (`ast` feature): a streaming visitor for the resolved inline list — the
/// allocation-free counterpart of [`list_to_tokens`]. Where `list_to_tokens`
/// materializes an owned [`SpanTok`] vector (one `String` per text span, cloned
/// payloads), [`emit_inline`] walks the same slot list and hands the consumer
/// *borrowed* `&str` slices, so a wire emitter can copy straight to its output
/// with no intermediate token vector and no per-span allocation.
///
/// Container nesting is expressed as `open`/`close` pairs (emphasis/strong/delete
/// and links alike); the consumer reconstructs the tree with its own stack.
/// Positions are the content byte range `[start, end)` (`start == u32::MAX` =
/// unknown → fall back to the block span). Adjacent text is already coalesced by
/// [`emit_inline`], so every [`text`](InlineSink::text) call is one whole run.
#[cfg(feature = "ast")]
pub trait InlineSink {
    fn text(&mut self, value: &str, start: u32, end: u32);
    fn open(&mut self, kind: &'static str, start: u32, end: u32);
    fn close(&mut self, start: u32, end: u32);
    fn code(&mut self, value: &str, start: u32, end: u32);
    fn html(&mut self, value: &str, start: u32, end: u32);
    fn brk(&mut self, start: u32, end: u32);
    fn image(&mut self, url: &str, title: Option<&str>, alt: &str, start: u32, end: u32);
    fn autolink(&mut self, url: &str, text: &str, start: u32, end: u32);
    fn link_open(&mut self, url: &str, title: Option<&str>, start: u32, end: u32);
    fn linkref_open(
        &mut self,
        identifier: &str,
        label: &str,
        reftype: &'static str,
        start: u32,
        end: u32,
    );
    fn imageref(
        &mut self,
        identifier: &str,
        label: &str,
        reftype: &'static str,
        alt: &str,
        start: u32,
        end: u32,
    );
    #[cfg(feature = "footnotes")]
    fn footnote_ref(&mut self, identifier: &str, label: &str, start: u32, end: u32);
    /// A remark-directive `textDirective`. `label` is a content byte range whose
    /// inline content forms the children (the wire emitter resolves it best-effort).
    #[cfg(feature = "directives")]
    fn text_directive(
        &mut self,
        name: &str,
        attrs: &[(String, String)],
        label: Option<(u32, u32)>,
        start: u32,
        end: u32,
    );
}

/// SPIKE (`ast`): drive an [`InlineSink`] over the resolved slot list, coalescing
/// runs of adjacent text into a single borrowed (or, only when ≥2 pieces must be
/// joined, buffered) `&str` — mirroring [`list_to_tokens`] node-for-node but
/// without owning anything.
#[cfg(feature = "ast")]
fn emit_inline<S: InlineSink>(list: &List, cur: &str, sem: &[Sem], sink: &mut S) {
    // The in-flight text run: borrowed while it stays a single piece, promoted to
    // an owned buffer (via `Cow::to_mut`) only when a second adjacent piece joins.
    let mut run: Option<(Cow<'_, str>, u32, u32)> = None;
    macro_rules! flush {
        () => {
            if let Some((s, st, en)) = run.take() {
                sink.text(&s, st, en); // non-empty: empty pieces never start a run
            }
        };
    }
    macro_rules! add_text {
        ($piece:expr, $st:expr, $en:expr) => {{
            let piece: Cow<'_, str> = $piece;
            if !piece.is_empty() {
                if let Some((buf, _, pe)) = run.as_mut() {
                    buf.to_mut().push_str(&piece);
                    *pe = $en;
                } else {
                    run = Some((piece, $st, $en));
                }
            }
        }};
    }

    let mut node = list.head;
    while let Some(idx) = node {
        let (s, e) = list.slots[idx].cspan;
        match &list.slots[idx].node {
            // `cur` already holds raw span text in wire mode — borrow it directly.
            Node::Span { start, end } => add_text!(Cow::Borrowed(&cur[*start..*end]), s, e),
            Node::Tag(t) => match *t {
                "<em>" => {
                    flush!();
                    sink.open("emphasis", s, e);
                }
                "</em>" => {
                    flush!();
                    sink.close(s, e);
                }
                "<strong>" => {
                    flush!();
                    sink.open("strong", s, e);
                }
                "</strong>" => {
                    flush!();
                    sink.close(s, e);
                }
                "<del>" => {
                    flush!();
                    sink.open("delete", s, e);
                }
                "</del>" => {
                    flush!();
                    sink.close(s, e);
                }
                // Unconsumed literal brackets stay as text.
                lit => add_text!(Cow::Borrowed(lit), s, e),
            },
            Node::Delim { ch, count, .. } => {
                let mut tmp = String::with_capacity(*count);
                for _ in 0..*count {
                    tmp.push(*ch as char);
                }
                add_text!(Cow::Owned(tmp), s, e);
            }
            Node::Sem(i) => {
                flush!();
                match &sem[*i as usize] {
                    Sem::Code(v) => sink.code(v, s, e),
                    Sem::LinkOpen { url, title } => sink.link_open(url, title.as_deref(), s, e),
                    Sem::Image { url, title, alt } => sink.image(url, title.as_deref(), alt, s, e),
                    Sem::Autolink { url, text } => sink.autolink(url, text, s, e),
                    Sem::Html(h) => sink.html(h, s, e),
                    Sem::Break => sink.brk(s, e),
                    Sem::LinkRef {
                        identifier,
                        label,
                        reftype,
                    } => sink.linkref_open(identifier, label, reftype, s, e),
                    Sem::ImageRef {
                        identifier,
                        label,
                        reftype,
                        alt,
                    } => sink.imageref(identifier, label, reftype, alt, s, e),
                    #[cfg(feature = "footnotes")]
                    Sem::FootnoteRef { identifier, label } => {
                        sink.footnote_ref(identifier, label, s, e)
                    }
                    #[cfg(feature = "directives")]
                    Sem::TextDirective { name, attrs, label } => {
                        sink.text_directive(name, attrs, *label, s, e)
                    }
                }
            }
            Node::LinkClose => {
                flush!();
                sink.close(s, e);
            }
        }
        node = list.slots[idx].next;
    }
    flush!();
}

struct Slot {
    node: Node,
    prev: Option<usize>,
    next: Option<usize>,
    /// SPIKE (`ast`): the content byte range `[start, end)` that produced this
    /// node, for unist `position`. `(0, 0)` until set in AST mode.
    #[cfg(feature = "ast")]
    cspan: (u32, u32),
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
            #[cfg(feature = "ast")]
            cspan: (u32::MAX, 0),
        });
        match self.tail {
            Some(t) => self.slots[t].next = Some(idx),
            None => self.head = Some(idx),
        }
        self.tail = Some(idx);
        idx
    }

    fn splice_after(&mut self, at: usize, node: Node) -> usize {
        let idx = self.slots.len();
        let next = self.slots[at].next;
        self.slots.push(Slot {
            node,
            prev: Some(at),
            next,
            #[cfg(feature = "ast")]
            cspan: (u32::MAX, 0),
        });
        self.slots[at].next = Some(idx);
        match next {
            Some(n) => self.slots[n].prev = Some(idx),
            None => self.tail = Some(idx),
        }
        idx
    }

    fn splice_before(&mut self, at: usize, node: Node) -> usize {
        let idx = self.slots.len();
        let prev = self.slots[at].prev;
        self.slots.push(Slot {
            node,
            prev,
            next: Some(at),
            #[cfg(feature = "ast")]
            cspan: (u32::MAX, 0),
        });
        self.slots[at].prev = Some(idx);
        match prev {
            Some(p) => self.slots[p].next = Some(idx),
            None => self.head = Some(idx),
        }
        idx
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
        // AST-mode only; never present on the HTML path.
        Node::Sem(_) | Node::LinkClose => {}
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
    /// SPIKE: side-table for [`Node::Sem`] semantic payloads. Unconditional (dead
    /// without `ast`) so `look_for_link_or_image`'s signature stays uncfg'd.
    sem: Vec<Sem>,
    /// SPIKE (`ast` feature): when `Some`, `render_inline` materializes owned
    /// [`InlineTok`] nodes here instead of emitting HTML to `out`. `None` (the
    /// default) is the unchanged fast path — the one extra branch it adds is only
    /// compiled in `ast` builds.
    /// PROTOTYPE (built-in heading-id transform): per-document slug→count map for
    /// github-slugger-style de-duplication. Persists across the whole render
    /// (cleared per document in `render_with`), unlike the per-paragraph `reset()`.
    pub(crate) slugs: std::collections::HashMap<String, u32>,
    /// GFM footnotes: the set of lowercased definition labels in the document
    /// (populated per-document by the renderer/mdast builder before walking, like
    /// `slugs`; survives `reset()`). A `[^label]` is a `footnoteReference` only if
    /// its lowercased label is in here — otherwise it stays literal text.
    #[cfg(feature = "footnotes")]
    pub(crate) footnote_ids: std::collections::HashSet<String>,
    /// GFM footnotes (HTML path): identifiers in first-reference order — a ref's
    /// number is its index here + 1. Document-level (cleared per document, like
    /// `slugs`); the footnotes `<section>` is emitted in this order.
    #[cfg(feature = "footnotes")]
    pub(crate) footnote_order: Vec<String>,
    /// GFM footnotes (HTML path): per-identifier count of references emitted so
    /// far, for the `fnref-id-2`, `-3`, … suffixes and the backref count.
    #[cfg(feature = "footnotes")]
    pub(crate) footnote_seen: std::collections::HashMap<String, u32>,
    #[cfg(feature = "ast")]
    toks: Option<Vec<SpanTok>>,
    /// SPIKE (`ast` feature): when `true`, `render_inline` resolves the inline
    /// list as usual but emits nothing and *leaves* `list`/`cur`/`sem` populated,
    /// so [`render_inline_to_sink`] can stream them through an [`InlineSink`] with
    /// no intermediate [`SpanTok`] vector. Mutually exclusive with `toks`.
    #[cfg(feature = "ast")]
    resolve: bool,
}

impl Scratch {
    pub fn new() -> Self {
        Scratch {
            list: List::new(),
            stack: Vec::with_capacity(16),
            cur: String::with_capacity(1024),
            norm: String::with_capacity(48),
            sem: Vec::new(),
            slugs: std::collections::HashMap::new(),
            #[cfg(feature = "footnotes")]
            footnote_ids: std::collections::HashSet::new(),
            #[cfg(feature = "footnotes")]
            footnote_order: Vec::new(),
            #[cfg(feature = "footnotes")]
            footnote_seen: std::collections::HashMap::new(),
            #[cfg(feature = "ast")]
            toks: None,
            #[cfg(feature = "ast")]
            resolve: false,
        }
    }
    fn reset(&mut self) {
        self.list.slots.clear();
        self.list.head = None;
        self.list.tail = None;
        self.stack.clear();
        self.cur.clear();
        self.sem.clear();
    }
}

/// Parse a GFM footnote reference `[^label]` at `bytes[i]` (`[`). Returns
/// `(label, bytes_consumed)`. Same label rules as a definition opener: non-empty,
/// no whitespace, no unescaped brackets (backslash escapes kept); ends at `]`.
#[cfg(feature = "footnotes")]
fn parse_footnote_ref(bytes: &[u8], i: usize) -> Option<(String, usize)> {
    if bytes.get(i + 1) != Some(&b'^') {
        return None;
    }
    let start = i + 2;
    let mut j = start;
    while j < bytes.len() {
        match bytes[j] {
            b']' => break,
            b'[' => return None,
            b' ' | b'\t' | b'\n' | b'\r' => return None,
            b'\\' if j + 1 < bytes.len() => j += 2,
            _ => j += 1,
        }
    }
    if j >= bytes.len() || bytes[j] != b']' || j == start {
        return None;
    }
    let label = std::str::from_utf8(&bytes[start..j]).ok()?.to_string();
    Some((label, j + 1 - i))
}

/// `encodeURIComponent(identifier)` — the exact transform `mdast-util-to-hast`
/// applies to a footnote identifier before building `fn-…`/`fnref-…` ids and
/// hrefs (keeps `A-Za-z0-9-_.!~*'()`, percent-encodes the rest as UTF-8).
#[cfg(feature = "footnotes")]
pub(crate) fn encode_footnote_id(id: &str) -> String {
    let mut out = String::with_capacity(id.len());
    for &b in id.as_bytes() {
        match b {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'!'
            | b'~'
            | b'*'
            | b'\''
            | b'('
            | b')' => out.push(b as char),
            _ => {
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
    out
}

/// Emit the remark-rehype `<sup>` markup for a footnote reference: the
/// document-order number (assigned on first sight) and the `-2`/`-3`… `fnref`
/// suffix per repeat occurrence. Mirrors `mdast-util-to-hast`'s reference handler.
#[cfg(feature = "footnotes")]
fn footnote_ref_html(
    id: &str,
    cur: &mut String,
    order: &mut Vec<String>,
    seen: &mut std::collections::HashMap<String, u32>,
) {
    let num = match order.iter().position(|x| x == id) {
        Some(p) => p + 1,
        None => {
            order.push(id.to_string());
            order.len()
        }
    };
    let count = {
        let c = seen.entry(id.to_string()).or_insert(0);
        *c += 1;
        *c
    };
    let safe = encode_footnote_id(id);
    cur.push_str("<sup><a href=\"#user-content-fn-");
    cur.push_str(&safe);
    cur.push_str("\" id=\"user-content-fnref-");
    cur.push_str(&safe);
    if count > 1 {
        cur.push('-');
        cur.push_str(&count.to_string());
    }
    cur.push_str("\" data-footnote-ref aria-describedby=\"footnote-label\">");
    cur.push_str(&num.to_string());
    cur.push_str("</a></sup>");
}

/// Stream inline content with no emphasis or link delimiters straight to
/// `out`: zero allocation, single pass, no node list. Mirrors the text-handling
/// arms of [`render_inline`] (kept in sync by the conformance suite).
// `HW` (hard_wraps) is a const generic so the default `HW = false` folds
// `if hard || HW` back to the original `if hard` — zero per-newline cost.
/// A GFM extended autolink may begin at text start or just after whitespace or
/// one of `*`, `_`, `~`, `(`.
fn al_boundary(b: &[u8], i: usize) -> bool {
    i == 0 || matches!(b[i - 1], b' ' | b'\t' | b'\n' | b'*' | b'_' | b'~' | b'(')
}

/// Trim trailing punctuation off a matched autolink (GFM §6.9): `?!.,:*_~`, an
/// unbalanced `)`, and a trailing entity reference `&…;`.
fn gfm_trim_url(b: &[u8], start: usize, mut end: usize) -> usize {
    while end > start {
        match b[end - 1] {
            b'?' | b'!' | b'.' | b',' | b':' | b'*' | b'_' | b'~' => end -= 1,
            b')' => {
                let opens = b[start..end].iter().filter(|&&x| x == b'(').count();
                let closes = b[start..end].iter().filter(|&&x| x == b')').count();
                if closes > opens {
                    end -= 1;
                } else {
                    break;
                }
            }
            b';' => {
                let mut j = end - 1;
                while j > start && b[j - 1].is_ascii_alphanumeric() {
                    j -= 1;
                }
                if j > start && b[j - 1] == b'&' {
                    end = j - 1;
                } else {
                    break;
                }
            }
            _ => break,
        }
    }
    end
}

/// If a `www.` / `http(s)://` autolink starts at `b[start]`, return its end.
fn gfm_scan_url(b: &[u8], start: usize) -> Option<usize> {
    let rest = &b[start..];
    let scan = if rest.starts_with(b"http://") {
        start + 7
    } else if rest.starts_with(b"https://") {
        start + 8
    } else if rest.starts_with(b"www.") {
        start + 4
    } else {
        return None;
    };
    // Domain: dot-separated labels of [A-Za-z0-9_-]; needs at least one dot.
    let mut i = scan;
    let mut dots = 0usize;
    while i < b.len() {
        match b[i] {
            b'.' => dots += 1,
            c if c.is_ascii_alphanumeric() || c == b'-' || c == b'_' => {}
            _ => break,
        }
        i += 1;
    }
    if i == scan || dots == 0 {
        return None;
    }
    // Path: up to whitespace or `<`.
    let mut end = i;
    while end < b.len() && !matches!(b[end], b' ' | b'\t' | b'\n' | b'\r' | b'<') {
        end += 1;
    }
    let end = gfm_trim_url(b, start, end);
    (end > scan && b[scan..end].contains(&b'.')).then_some(end)
}

/// At a `:` opening `://`, if a preceding `http`/`https` scheme sits at an
/// autolink boundary, return the URL `(start, end)`. Lets the scan trigger on the
/// rare `:` instead of every `h`.
fn gfm_scan_url_at_colon(b: &[u8], i: usize) -> Option<(usize, usize)> {
    if b.get(i + 1) != Some(&b'/') || b.get(i + 2) != Some(&b'/') {
        return None;
    }
    let start = if i >= 5 && &b[i - 5..i] == b"https" {
        i - 5
    } else if i >= 4 && &b[i - 4..i] == b"http" {
        i - 4
    } else {
        return None;
    };
    if !al_boundary(b, start) {
        return None;
    }
    gfm_scan_url(b, start).map(|end| (start, end))
}

/// If a bare email autolink ends at the `@` `b[at]`, return `(localpart start,
/// end)`. The local part must sit at an autolink boundary.
fn gfm_scan_email(b: &[u8], at: usize) -> Option<(usize, usize)> {
    let mut s = at;
    while s > 0
        && matches!(b[s - 1], b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'.' | b'_' | b'+' | b'-')
    {
        s -= 1;
    }
    if s == at || !al_boundary(b, s) {
        return None;
    }
    // Domain: [A-Za-z0-9_-] labels separated by `.`, at least one dot, ending on
    // an alphanumeric (a trailing `.`/`-`/`_` is not part of the address).
    let mut i = at + 1;
    let dstart = i;
    while i < b.len() {
        match b[i] {
            b'.' | b'-' | b'_' => {}
            c if c.is_ascii_alphanumeric() => {}
            _ => break,
        }
        i += 1;
    }
    let mut e = i;
    while e > dstart && !b[e - 1].is_ascii_alphanumeric() {
        e -= 1;
    }
    (e > dstart && b[dstart..e].contains(&b'.')).then_some((s, e))
}

/// Emit a `www.`/`http(s)://` autolink as `<a href="…">…</a>` (www gets an
/// `http://` href prefix; the visible text is verbatim).
fn emit_url(src: &str, start: usize, end: usize, out: &mut String) {
    let text = &src[start..end];
    out.push_str("<a href=\"");
    if !text.starts_with("http") {
        out.push_str("http://");
    }
    escape_href(text, out);
    out.push_str("\">");
    escape_html(text, out);
    out.push_str("</a>");
}

/// The mdast `link` url for a GFM extended URL autolink: a `www.` link gets an
/// `http://` scheme prefix, an explicit `http(s)://` is kept verbatim (mirrors
/// the href [`emit_url`] writes, but unescaped — mdast carries the raw url).
/// Ungated like [`emit_url`]/[`emit_email`]: the call sites live in `if ast_mode`
/// (a runtime `const false` without `ast`) blocks that are still compiled and
/// name-resolved, so the fn must exist in every feature configuration.
fn gfm_url_href(text: &str) -> String {
    if text.starts_with("http") {
        text.to_owned()
    } else {
        format!("http://{text}")
    }
}

/// Emit a bare email autolink as `<a href="mailto:…">…</a>`.
fn emit_email(src: &str, start: usize, end: usize, out: &mut String) {
    let email = &src[start..end];
    out.push_str("<a href=\"mailto:");
    escape_href(email, out);
    out.push_str("\">");
    escape_html(email, out);
    out.push_str("</a>");
}

/// Like [`stream_inline`] but also recognizes GFM extended autolinks (bare
/// `www.`/`http(s)://`/email) in the streamed text. A separate function so the
/// default fast path stays byte-identical; only reached when `autolink` is on.
fn stream_autolink(src: &str, out: &mut String, hw: bool, tf: bool) {
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
                    if tf {
                        crate::render::filter_html(&src[i..end], out);
                    } else {
                        out.push_str(&src[i..end]);
                    }
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
                out.push_str(if hard || hw { "<br />\n" } else { "\n" });
                i = skip_spaces(bytes, i + 1);
                run = i;
            }
            b'@' => {
                if let Some((s, e)) = gfm_scan_email(bytes, i) {
                    escape_html(&src[run..s], out);
                    emit_email(src, s, e, out);
                    i = e;
                    run = i;
                } else {
                    i += 1;
                }
            }
            b'w' | b'W' if al_boundary(bytes, i) => {
                if let Some(end) = gfm_scan_url(bytes, i) {
                    escape_html(&src[run..i], out);
                    emit_url(src, i, end, out);
                    i = end;
                    run = i;
                } else {
                    i += 1;
                }
            }
            b':' => {
                if let Some((s, e)) = gfm_scan_url_at_colon(bytes, i) {
                    escape_html(&src[run..s], out);
                    emit_url(src, s, e, out);
                    i = e;
                    run = i;
                } else {
                    i += 1;
                }
            }
            // SIMD-skip to the next special or autolink trigger.
            _ => i += 1 + find_stream_al(&bytes[i + 1..]).unwrap_or(bytes.len() - i - 1),
        }
    }
    escape_html(&src[run..], out);
}

fn stream_inline<const HW: bool>(src: &str, out: &mut String, tf: bool) {
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
                    if tf {
                        crate::render::filter_html(&src[i..end], out);
                    } else {
                        out.push_str(&src[i..end]);
                    }
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
                out.push_str(if hard || HW { "<br />\n" } else { "\n" });
                i = skip_spaces(bytes, i + 1);
                run = i;
            }
            // Skip plain text to the next significant byte in one SIMD pass.
            _ => i += 1 + find_stream(&bytes[i + 1..]).unwrap_or(bytes.len() - i - 1),
        }
    }
    escape_html(src[run..].trim_end_matches(' '), out);
}

/// Parse `src` (a block's raw inline text) to HTML, appending to `out`. Picks
/// the monomorphized inline renderer once per call so per-byte options
/// (`hard_wraps`, `strikethrough`) resolved at this boundary cost nothing in the
/// byte loop — disabled flags fold the gfm tables and the `~` arm away.
///
/// `inline`: this dispatcher is tiny (one match → a monomorphized impl); keeping
/// it inlined at every call site keeps a new caller (e.g. the task-list renderer)
/// from flipping the heuristic and adding a call per paragraph (~0.5%).
#[inline]
pub fn render_inline(
    src: &str,
    out: &mut String,
    refmap: &RefMap,
    scratch: &mut Scratch,
    opts: Options,
) {
    // `Options::GFM &&` folds these to `false` when the `gfm` feature is off, so
    // the gfm scan tables, the `~`/autolink arms, and the tagfilter call are all
    // eliminated and the default build streams pure CommonMark.
    let tf = Options::GFM && opts.tagfilter;
    let al = Options::GFM && opts.autolink;
    let fno = Options::FOOTNOTES && opts.footnotes;
    let emo = Options::EMOJI && opts.emoji;
    let dir = Options::DIRECTIVES && opts.directives;
    match (opts.hard_wraps, Options::GFM && opts.strikethrough) {
        (false, false) => render_inline_impl::<false, false>(
            src, out, refmap, scratch, tf, al, fno, emo, dir, opts,
        ),
        (false, true) => render_inline_impl::<false, true>(
            src, out, refmap, scratch, tf, al, fno, emo, dir, opts,
        ),
        (true, false) => render_inline_impl::<true, false>(
            src, out, refmap, scratch, tf, al, fno, emo, dir, opts,
        ),
        (true, true) => {
            render_inline_impl::<true, true>(src, out, refmap, scratch, tf, al, fno, emo, dir, opts)
        }
    }
}

/// Emoji shortcode lookup, present only with the `emoji` feature (folds to `None`
/// otherwise so the call site compiles in every build).
#[cfg(feature = "emoji")]
#[inline]
fn emoji_lookup(bytes: &[u8], i: usize) -> Option<(&'static str, usize)> {
    crate::ext::emoji::lookup(bytes, i)
}
#[cfg(not(feature = "emoji"))]
#[inline]
fn emoji_lookup(_bytes: &[u8], _i: usize) -> Option<(&'static str, usize)> {
    None
}

/// SPIKE (`ast` feature): parse `src`'s inline content into owned [`InlineTok`]
/// nodes instead of HTML. Runs the exact same pipeline as [`render_inline`]
/// (text/code/links, emphasis resolution) but captures the resolved node list.
#[cfg(feature = "ast")]
pub fn render_inline_to_tokens(
    src: &str,
    refmap: &RefMap,
    scratch: &mut Scratch,
    opts: Options,
) -> Vec<SpanTok> {
    scratch.toks = Some(Vec::new());
    let mut sink = String::new(); // unused in token mode
    render_inline(src, &mut sink, refmap, scratch, opts);
    scratch.toks.take().unwrap_or_default()
}

/// SPIKE (`ast` feature): parse `src`'s inline content and stream it through
/// `sink` with no intermediate [`SpanTok`] vector. Runs the same resolution as
/// [`render_inline_to_tokens`], but instead of materializing owned tokens it
/// leaves the resolved list in `scratch` and walks it with [`emit_inline`],
/// handing the sink borrowed slices. This is the allocation-light path the binary
/// wire emitter uses.
#[cfg(feature = "ast")]
pub fn render_inline_to_sink<S: InlineSink>(
    src: &str,
    refmap: &RefMap,
    scratch: &mut Scratch,
    opts: Options,
    sink: &mut S,
) {
    scratch.resolve = true;
    let mut unused = String::new(); // no HTML emitted in resolve mode
    render_inline(src, &mut unused, refmap, scratch, opts);
    scratch.resolve = false;
    // `render_inline` left the resolved nodes in place; walk them into the sink.
    emit_inline(&scratch.list, &scratch.cur, &scratch.sem, sink);
}

#[allow(unused_assignments)] // `seg` is updated at segment ends; the last is unused
#[allow(clippy::too_many_arguments)] // hot inner loop; bundling args would add indirection
fn render_inline_impl<const HW: bool, const ST: bool>(
    src: &str,
    out: &mut String,
    refmap: &RefMap,
    scratch: &mut Scratch,
    tf: bool,
    al: bool,
    // Without the `footnotes` feature its only consumer (the `[^…]` detection in
    // the `b'['` arm) is cfg'd out, so the bool is threaded but unread.
    #[cfg_attr(not(feature = "footnotes"), allow(unused_variables))] fno: bool,
    emo: bool,
    dir: bool,
    // Without the `directives` feature its only consumer (the inline text-directive
    // sub-render in the `b':'` arm) is cfg'd out, so it may be threaded but unread.
    #[cfg_attr(not(feature = "directives"), allow(unused_variables))] opts: Options,
) {
    let bytes = src.as_bytes();
    // SPIKE: AST mode captures semantic nodes instead of HTML. `ast_mode` is a
    // compile-time `false` without the `ast` feature, so every `if ast_mode {…}`
    // branch below is dead-code-eliminated and the fast path is untouched.
    #[cfg(feature = "ast")]
    let ast_mode = scratch.toks.is_some() || scratch.resolve;
    // Captured before the `list`/`cur`/`sem` borrows below so it can be read at the
    // emission tail without re-borrowing `scratch`.
    #[cfg(feature = "ast")]
    let resolve = scratch.resolve;
    #[cfg(not(feature = "ast"))]
    let ast_mode = false;
    // SPIKE (`ast`): wire/AST mode writes *raw* (un-escaped) text into the span
    // buffer `cur`, so the wire reader borrows it with no un-escape round-trip.
    // Identical to `ast_mode` (resolve || toks), captured as a plain `bool` here —
    // before `cur` is borrowed below — so the emit helpers need no `scratch`
    // re-borrow. Const-folds to `false` (HTML, byte-identical) without `ast`.
    let raw = ast_mode;
    // Fast path: no emphasis/link (or `~` when ST) delimiters → stream directly.
    // Skipped in AST mode: the streaming path emits HTML for code spans /
    // autolinks / raw HTML, which we must instead capture as semantic nodes.
    let gate = if ST {
        find_emph_gfm(bytes)
    } else {
        find_emph(bytes)
    };
    // Emoji and text directives need the node path only when a `:` is actually
    // present; a `:`-free line still takes the fast stream path.
    let emo_here = (emo || dir) && memchr1(bytes, b':').is_some();
    if gate.is_none() && !ast_mode && !emo_here {
        if al {
            stream_autolink(src, out, HW, tf);
        } else {
            stream_inline::<HW>(src, out, tf);
        }
        return;
    }
    scratch.reset();
    // GFM footnotes: the definition set (read-only) and document-order numbering
    // state (HTML path). Distinct fields from the borrows below, so they coexist;
    // untouched by `reset()` (document-level, like `slugs`).
    #[cfg(feature = "footnotes")]
    let fn_ids = &scratch.footnote_ids;
    #[cfg(feature = "footnotes")]
    let fn_order = &mut scratch.footnote_order;
    #[cfg(feature = "footnotes")]
    let fn_seen = &mut scratch.footnote_seen;
    let list = &mut scratch.list;
    let stack = &mut scratch.stack;
    let cur = &mut scratch.cur;
    let norm = &mut scratch.norm;
    let sem = &mut scratch.sem;
    let mut i = 0usize;
    let mut run = 0usize;
    let mut seg = 0usize; // start (in `cur`) of the open text segment
    // SPIKE (`ast`): start (in `src` content) of the open text segment, for the
    // text node's unist `position`.
    #[cfg(feature = "ast")]
    let mut cseg = 0usize;

    // Close the open text segment into a Span node. `$cend` is the content byte
    // offset where the text ends (AST mode records `[cseg, $cend)` on the span;
    // the HTML path ignores it).
    macro_rules! flush {
        ($cend:expr) => {{
            if cur.len() > seg {
                let _id = list.push(Node::Span {
                    start: seg,
                    end: cur.len(),
                });
                #[cfg(feature = "ast")]
                {
                    list.slots[_id].cspan = (cseg as u32, ($cend) as u32);
                }
                seg = cur.len();
            }
            #[cfg(feature = "ast")]
            {
                cseg = ($cend) as usize;
            }
        }};
    }

    // SPIKE (`ast`): set the content span on the node just pushed at `$idx` and
    // advance the text-segment cursor. A no-op without the `ast` feature (the
    // call sites live inside runtime `if ast_mode` blocks that still compile).
    macro_rules! cspan {
        ($idx:expr, $s:expr, $e:expr) => {{
            #[cfg(feature = "ast")]
            {
                list.slots[$idx].cspan = ($s as u32, $e as u32);
                cseg = ($e) as usize;
            }
            #[cfg(not(feature = "ast"))]
            {
                let _ = ($idx, $s, $e);
            }
        }};
    }

    while i < bytes.len() {
        match bytes[i] {
            b'\\' => {
                emit_text(raw, &src[run..i], cur);
                if i + 1 < bytes.len() && bytes[i + 1] == b'\n' {
                    if ast_mode {
                        flush!(i);
                        let si = sem.len() as u32;
                        sem.push(Sem::Break);
                        let bid = list.push(Node::Sem(si));
                        // The break spans the `\` and the line ending.
                        cspan!(bid, i, i + 2);
                        seg = cur.len();
                        i = skip_spaces(bytes, i + 2);
                        #[cfg(feature = "ast")]
                        {
                            cseg = i;
                        }
                    } else {
                        cur.push_str("<br />\n");
                        i = skip_spaces(bytes, i + 2);
                    }
                } else if i + 1 < bytes.len() && is_ascii_punct(bytes[i + 1]) {
                    emit_escaped_byte(raw, cur, bytes[i + 1]);
                    i += 2;
                } else {
                    cur.push('\\');
                    i += 1;
                }
                run = i;
            }
            b'`' => {
                emit_text(raw, &src[run..i], cur);
                if ast_mode {
                    if let Some((val, new_i)) = code_span_value(src, bytes, i) {
                        flush!(i);
                        let si = sem.len() as u32;
                        sem.push(Sem::Code(val));
                        let cid = list.push(Node::Sem(si));
                        cspan!(cid, i, new_i);
                        seg = cur.len();
                        i = new_i;
                    } else {
                        let n = bytes[i..].iter().take_while(|&&b| b == b'`').count();
                        cur.push_str(&src[i..i + n]);
                        i += n;
                    }
                } else if let Some(new_i) = try_code_span(src, bytes, i, cur) {
                    i = new_i;
                } else {
                    let n = bytes[i..].iter().take_while(|&&b| b == b'`').count();
                    cur.push_str(&src[i..i + n]);
                    i += n;
                }
                run = i;
            }
            b'&' => {
                emit_text(raw, &src[run..i], cur);
                if let Some((val, consumed)) = parse_entity(bytes, i) {
                    match val {
                        Resolved::Cp(cp) => emit_char_escaped(raw, cur, cp),
                        Resolved::Text(s) => emit_str_escaped(raw, cur, s),
                    }
                    i += consumed;
                } else {
                    // A bare `&` (no entity): raw text or its `&amp;` escape.
                    if raw {
                        cur.push('&');
                    } else {
                        cur.push_str("&amp;");
                    }
                    i += 1;
                }
                run = i;
            }
            b'<' => {
                emit_text(raw, &src[run..i], cur);
                if let Some((consumed, html)) = try_autolink(src, bytes, i) {
                    if ast_mode {
                        // Recover url/text: href is percent-encoded (lossy), but
                        // the visible text is the original (only HTML-escaped).
                        let close = bytes[i + 1..].iter().position(|&b| b == b'>').unwrap();
                        let content = &src[i + 1..i + 1 + close];
                        let url = if is_uri_autolink(content) {
                            content.to_owned()
                        } else {
                            format!("mailto:{content}")
                        };
                        flush!(i);
                        let si = sem.len() as u32;
                        sem.push(Sem::Autolink {
                            url,
                            text: content.to_owned(),
                        });
                        let aid = list.push(Node::Sem(si));
                        cspan!(aid, i, i + consumed);
                        seg = cur.len();
                    } else {
                        cur.push_str(&html);
                    }
                    i += consumed;
                } else if let Some(end) = try_raw_html(bytes, i) {
                    if ast_mode {
                        flush!(i);
                        let si = sem.len() as u32;
                        sem.push(Sem::Html(src[i..end].to_owned()));
                        let hid = list.push(Node::Sem(si));
                        cspan!(hid, i, end);
                        seg = cur.len();
                    } else if tf {
                        crate::render::filter_html(&src[i..end], cur);
                    } else {
                        cur.push_str(&src[i..end]); // verbatim
                    }
                    i = end;
                } else {
                    // A bare `<` (no autolink/raw HTML): raw text or its `&lt;`.
                    if raw {
                        cur.push('<');
                    } else {
                        cur.push_str("&lt;");
                    }
                    i += 1;
                }
                run = i;
            }
            b'*' | b'_' => {
                let ch = bytes[i];
                emit_text(raw, &src[run..i], cur);
                flush!(i);
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
                #[cfg(feature = "ast")]
                cspan!(idx, i, i + count);
                stack.push(StackItem::Emph(idx));
                i += count;
                run = i;
            }
            // GFM strikethrough: a `~` run of 1 or 2 is a delimiter (→ <del>);
            // 3+ tildes are literal. Only reachable when ST is on.
            b'~' if ST => {
                emit_text(raw, &src[run..i], cur);
                flush!(i);
                let count = bytes[i..].iter().take_while(|&&b| b == b'~').count();
                if count <= 2 {
                    let before = src[..i].chars().next_back();
                    let after = src[i + count..].chars().next();
                    let (can_open, can_close) = flanking(b'~', before, after);
                    let idx = list.push(Node::Delim {
                        ch: b'~',
                        count,
                        orig: count,
                        can_open,
                        can_close,
                    });
                    // SPIKE (`ast`): record the run's byte span like `*`/`_` do —
                    // the strikethrough match below shrinks it by `use_delims`,
                    // which underflows if the span is left at its `(MAX, 0)` default.
                    #[cfg(feature = "ast")]
                    cspan!(idx, i, i + count);
                    stack.push(StackItem::Emph(idx));
                } else {
                    cur.push_str(&src[i..i + count]);
                }
                i += count;
                run = i;
            }
            b'[' => {
                // GFM footnote reference `[^label]` whose lowercased label has a
                // matching definition; otherwise fall through to a link bracket.
                // Without the `footnotes` feature this whole detection is cfg'd
                // out and every `[` falls straight through to the link bracket.
                #[cfg(feature = "footnotes")]
                let fnref = if fno {
                    parse_footnote_ref(bytes, i).and_then(|(label, consumed)| {
                        let id = label.to_lowercase();
                        fn_ids.contains(&id).then_some((id, label, consumed))
                    })
                } else {
                    None
                };
                #[cfg(feature = "footnotes")]
                let handled = if let Some((id, label, consumed)) = fnref {
                    emit_text(raw, &src[run..i], cur);
                    if ast_mode {
                        flush!(i);
                        let si = sem.len() as u32;
                        sem.push(Sem::FootnoteRef {
                            identifier: id,
                            label,
                        });
                        let fid = list.push(Node::Sem(si));
                        cspan!(fid, i, i + consumed);
                        seg = cur.len();
                    } else {
                        footnote_ref_html(&id, cur, fn_order, fn_seen);
                    }
                    i += consumed;
                    run = i;
                    true
                } else {
                    false
                };
                #[cfg(not(feature = "footnotes"))]
                let handled = false;
                if !handled {
                    emit_text(raw, &src[run..i], cur);
                    flush!(i);
                    let node = list.push(Node::Tag("["));
                    #[cfg(feature = "ast")]
                    cspan!(node, i, i + 1);
                    stack.push(StackItem::Bracket {
                        node,
                        image: false,
                        active: true,
                        text_src: i + 1,
                    });
                    i += 1;
                    run = i;
                }
            }
            b'!' if bytes.get(i + 1) == Some(&b'[') => {
                emit_text(raw, &src[run..i], cur);
                flush!(i);
                let node = list.push(Node::Tag("!["));
                #[cfg(feature = "ast")]
                cspan!(node, i, i + 2);
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
                emit_text(raw, &src[run..i], cur);
                flush!(i);
                let rb = list.push(Node::Tag("]"));
                #[cfg(feature = "ast")]
                cspan!(rb, i, i + 1);
                let rb_src = i;
                i += 1;
                look_for_link_or_image(
                    src, bytes, &mut i, list, stack, cur, norm, refmap, rb, rb_src, ast_mode, sem,
                );
                // A resolved link/image appended its tag to `cur` and spanned it
                // directly; the next text segment starts after it.
                seg = cur.len();
                #[cfg(feature = "ast")]
                {
                    cseg = i;
                }
                run = i;
            }
            b'\n' => {
                // Trailing spaces in the pending run decide the break kind. mdast and
                // cmark diverge on tabs: cmark keeps a soft break's trailing tabs and
                // lets ≥2 trailing spaces force a hard break even past an adjacent tab
                // (`a\t  \n` is hard); mdast drops trailing tabs from the text and a
                // tab in the trailing run cancels the hard break (`a\t  \n` is soft).
                let line = &src[run..i];
                let sp = line.trim_end_matches(' ');
                let hard = line.len() - sp.len() >= 2 && (!ast_mode || !sp.ends_with('\t'));
                let trimmed = if ast_mode {
                    line.trim_end_matches([' ', '\t'])
                } else {
                    sp
                };
                emit_text(raw, trimmed, cur);
                if (hard || HW) && ast_mode {
                    // The text node ends before the trailing spaces; the break
                    // spans them through the line ending.
                    let text_end = run + trimmed.len();
                    flush!(text_end);
                    let si = sem.len() as u32;
                    sem.push(Sem::Break);
                    let brk = list.push(Node::Sem(si));
                    cspan!(brk, text_end, i + 1);
                    seg = cur.len();
                    i = skip_spaces(bytes, i + 1);
                    #[cfg(feature = "ast")]
                    {
                        cseg = i;
                    }
                } else {
                    // Soft break stays as text "\n"; in AST mode it is folded into
                    // the surrounding text node. When the segment is still empty,
                    // any trailing spaces dropped here would otherwise become the
                    // next text node's *leading* bytes — start it at the newline.
                    #[cfg(feature = "ast")]
                    if cur.len() == seg {
                        cseg = i;
                    }
                    cur.push_str(if hard || HW { "<br />\n" } else { "\n" });
                    i = skip_spaces(bytes, i + 1);
                }
                run = i;
            }
            // GFM autolinks in delimiter-run text (when on). The URL trigger
            // fires at the start, so `gfm_scan_url` swallows the whole URL —
            // including any `_`/`*` in the path — before they become delimiters.
            b'@' if al => {
                if let Some((s, e)) = gfm_scan_email(bytes, i) {
                    emit_text(raw, &src[run..s], cur);
                    if ast_mode {
                        // GFM extended email autolink → mdast `link` (mailto: href,
                        // verbatim text child) — the same node a `<addr>` autolink
                        // yields, instead of leaking `<a …>` HTML into a text node.
                        flush!(s);
                        let text = src[s..e].to_owned();
                        let si = sem.len() as u32;
                        sem.push(Sem::Autolink {
                            url: format!("mailto:{text}"),
                            text,
                        });
                        let aid = list.push(Node::Sem(si));
                        cspan!(aid, s, e);
                        seg = cur.len();
                    } else {
                        emit_email(src, s, e, cur);
                    }
                    i = e;
                    run = i;
                } else {
                    i += 1;
                }
            }
            b'w' | b'W' if al && al_boundary(bytes, i) => {
                if let Some(end) = gfm_scan_url(bytes, i) {
                    emit_text(raw, &src[run..i], cur);
                    if ast_mode {
                        // GFM extended `www.` autolink → mdast `link`.
                        flush!(i);
                        let text = src[i..end].to_owned();
                        let url = gfm_url_href(&text);
                        let si = sem.len() as u32;
                        sem.push(Sem::Autolink { url, text });
                        let aid = list.push(Node::Sem(si));
                        cspan!(aid, i, end);
                        seg = cur.len();
                    } else {
                        emit_url(src, i, end, cur);
                    }
                    i = end;
                    run = i;
                } else {
                    i += 1;
                }
            }
            b':' if al || emo || dir => {
                if al && let Some((s, e)) = gfm_scan_url_at_colon(bytes, i) {
                    emit_text(raw, &src[run..s], cur);
                    if ast_mode {
                        // GFM extended `http(s)://` autolink → mdast `link`.
                        flush!(s);
                        let text = src[s..e].to_owned();
                        let url = gfm_url_href(&text);
                        let si = sem.len() as u32;
                        sem.push(Sem::Autolink { url, text });
                        let aid = list.push(Node::Sem(si));
                        cspan!(aid, s, e);
                        seg = cur.len();
                    } else {
                        emit_url(src, s, e, cur);
                    }
                    i = e;
                    run = i;
                } else if emo && let Some((emoji, e)) = emoji_lookup(bytes, i) {
                    // Emoji is plain text in both HTML and AST mode: flush the
                    // pending text and append the emoji to the current segment
                    // (no node boundary), so it folds into the surrounding text.
                    emit_text(raw, &src[run..i], cur);
                    cur.push_str(emoji);
                    i = e;
                    run = i;
                } else {
                    // remark-directive inline `:name[label]{attrs}`. Without the
                    // `directives` feature `dir` const-folds to `false` and this
                    // whole detection is cfg'd out, so every other `:` advances by
                    // one byte (the pure-CommonMark fast path).
                    #[cfg(feature = "directives")]
                    let handled = if dir
                        && !(i > 0 && bytes[i - 1] == b':')
                        && bytes.get(i + 1) != Some(&b':')
                        && let Some(h) = crate::directive::parse_header(&bytes[i + 1..])
                        // A trailing `:` means this is a `:shortcode:`-shaped token,
                        // not a text directive (matches micromark-extension-directive).
                        && bytes.get(i + 1 + h.consumed) != Some(&b':')
                    {
                        let ac = i + 1;
                        let after = ac + h.consumed;
                        let name = src[ac + h.name_start..ac + h.name_end].to_owned();
                        let label = h.label.map(|(ls, le)| (ac + ls, ac + le));
                        emit_text(raw, &src[run..i], cur);
                        if ast_mode {
                            flush!(i);
                            let si = sem.len() as u32;
                            sem.push(Sem::TextDirective {
                                name,
                                attrs: h.attrs,
                                label: label.map(|(s, e)| (s as u32, e as u32)),
                            });
                            let did = list.push(Node::Sem(si));
                            cspan!(did, i, after);
                            seg = cur.len();
                        } else {
                            // HTML convention: <name attrs>label</name>; the label is
                            // inline content (sub-rendered with a scratch of its own).
                            crate::render::directive_open_tag(&name, &h.attrs, cur);
                            if let Some((ls, le)) = label {
                                // The label is inline content; sub-render it with a
                                // scratch of its own (the live one is mid-parse).
                                let mut tmp = Scratch::new();
                                let mut html = String::new();
                                render_inline(&src[ls..le], &mut html, refmap, &mut tmp, opts);
                                cur.push_str(&html);
                            }
                            cur.push_str("</");
                            cur.push_str(&name);
                            cur.push('>');
                        }
                        i = after;
                        run = i;
                        true
                    } else {
                        false
                    };
                    #[cfg(not(feature = "directives"))]
                    let handled = false;
                    if !handled {
                        i += 1;
                    }
                }
            }
            // Skip plain text to the next significant byte in one SIMD pass —
            // SIMD-skip to the next special — when autolink is on, the set also
            // includes the `w`/`h`/`@` triggers handled by the arms above.
            _ => {
                let rest = &bytes[i + 1..];
                let skip = if ST {
                    if al {
                        find_inline_gfm_al(rest)
                    } else {
                        find_inline_gfm(rest)
                    }
                } else if al {
                    find_inline_al(rest)
                } else {
                    find_inline(rest)
                };
                let mut adv = 1 + skip.unwrap_or(bytes.len() - i - 1);
                // Emoji and text directives: `:` is not in the (non-autolink) scan
                // set, so clamp the skip to the next `:` here when either is on.
                if (emo || dir)
                    && let Some(c) = memchr1(&bytes[i + 1..i + adv], b':')
                {
                    adv = 1 + c;
                }
                i += adv;
            }
        }
    }
    // Trailing whitespace at the very end of a block is dropped (no following line
    // to form a hard break). The last text node ends before it; mdast trims trailing
    // spaces *and tabs* from the final text node's value *and* position, while the
    // cmark HTML path keeps tabs here (matching its block-final handling elsewhere).
    let block_text_end = run
        + src[run..]
            .trim_end_matches(|c| c == ' ' || (ast_mode && c == '\t'))
            .len();
    emit_text(raw, &src[run..block_text_end], cur);
    flush!(block_text_end);

    process_emphasis(list, stack, 0);
    // SPIKE: in AST mode, capture semantic nodes instead of rendering to `out`.
    #[cfg(feature = "ast")]
    if ast_mode {
        // Sink mode: leave `list`/`cur`/`sem` populated; the caller
        // (`render_inline_to_sink`) streams them through an `InlineSink` with no
        // owned token vector.
        if resolve {
            return;
        }
        // Token mode: materialize the owned `SpanTok` vector. The `list`/`cur`
        // borrows end at the `list_to_tokens` call, so the disjoint `scratch.toks`
        // field can be assigned right after.
        let mut v = Vec::new();
        list_to_tokens(list, cur, sem, &mut v);
        scratch.toks = Some(v);
        return;
    }
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
    ast_mode: bool,
    sem: &mut Vec<Sem>,
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
    let mut ref_info: Option<RefInfo> = None;
    let Some((dest_raw, title_raw, new_i)) =
        parse_link_target(src, bytes, *i, refmap, text, norm, &mut ref_info)
    else {
        stack.remove(op);
        return; // ] stays literal
    };

    // Resolve emphasis within the link text (bounded below by the opener).
    process_emphasis(list, stack, op + 1);

    if image {
        if ast_mode {
            // mdast `image`/`imageReference` is a leaf whose `alt` is the
            // *plain text* of the link text — including nested image alts (which
            // are `Sem` nodes the HTML renderer would drop).
            let alt = ast_image_alt(list, op_node, rb_node, cur, sem);
            let si = sem.len() as u32;
            sem.push(match ref_info {
                Some(ri) => Sem::ImageRef {
                    identifier: ri.identifier,
                    label: ri.label,
                    reftype: ri.reftype,
                    alt,
                },
                None => Sem::Image {
                    url: unescape_string(dest_raw).into_owned(),
                    title: title_raw.map(|t| unescape_string(t).into_owned()),
                    alt,
                },
            });
            list.slots[op_node].node = Node::Sem(si);
            #[cfg(feature = "ast")]
            {
                list.slots[op_node].cspan = ((text_src - 2) as u32, new_i as u32);
            }
        } else {
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
        }

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
        if ast_mode {
            let si = sem.len() as u32;
            sem.push(match ref_info {
                Some(ri) => Sem::LinkRef {
                    identifier: ri.identifier,
                    label: ri.label,
                    reftype: ri.reftype,
                },
                None => Sem::LinkOpen {
                    url: unescape_string(dest_raw).into_owned(),
                    title: title_raw.map(|t| unescape_string(t).into_owned()),
                },
            });
            list.slots[op_node].node = Node::Sem(si);
            list.slots[rb_node].node = Node::LinkClose;
            #[cfg(feature = "ast")]
            {
                // The LinkOpen carries the whole link's span (`[`…end).
                list.slots[op_node].cspan = ((text_src - 1) as u32, new_i as u32);
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
        }

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

/// SPIKE (AST mode): the *plain text* of an image's link text (the slots between
/// `op_node` and `rb_node`), for mdast `image.alt`. Mirrors [`list_to_tokens`]'s
/// text projection: emphasis markers contribute nothing, `Sem` leaves contribute
/// their plain value (a nested image its `alt`, code its value, …). Defined
/// unconditionally (dead without `ast`) so the `if ast_mode` branch compiles.
#[allow(dead_code)]
fn ast_image_alt(list: &List, op_node: usize, rb_node: usize, cur: &str, sem: &[Sem]) -> String {
    let mut s = String::new();
    let mut node = list.slots[op_node].next;
    while let Some(idx) = node {
        if idx == rb_node {
            break;
        }
        match &list.slots[idx].node {
            // `cur` already holds raw span text in wire mode — borrow it directly.
            Node::Span { start, end } => s.push_str(&cur[*start..*end]),
            Node::Tag(t) => match *t {
                "<em>" | "</em>" | "<strong>" | "</strong>" | "<del>" | "</del>" => {}
                lit => s.push_str(lit),
            },
            Node::Delim { ch, count, .. } => {
                for _ in 0..*count {
                    s.push(*ch as char);
                }
            }
            Node::Sem(i) => match &sem[*i as usize] {
                Sem::Image { alt, .. } | Sem::ImageRef { alt, .. } => s.push_str(alt),
                Sem::Code(v) => s.push_str(v),
                Sem::Autolink { text, .. } => s.push_str(text),
                Sem::Html(h) => s.push_str(h),
                #[cfg(feature = "footnotes")]
                Sem::FootnoteRef { .. } => {}
                #[cfg(feature = "directives")]
                Sem::TextDirective { .. } => {}
                Sem::LinkOpen { .. } | Sem::LinkRef { .. } | Sem::Break => {}
            },
            Node::LinkClose => {}
        }
        node = list.slots[idx].next;
    }
    s
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
    ref_out: &mut Option<RefInfo>,
) -> Option<(&'a str, Option<&'a str>, usize)> {
    if bytes.get(i) == Some(&b'(')
        && let Some(r) = parse_inline_paren(src, bytes, i)
    {
        return Some(r); // inline target: not a reference, `ref_out` stays None
    }
    // Full/collapsed reference: [label] / []
    if bytes.get(i) == Some(&b'[') {
        if let Some((label, end)) = read_bracket_label(src, bytes, i) {
            // collapsed `[]` reuses the link text as the label.
            let collapsed = label.trim().is_empty();
            let lab = if collapsed { text } else { label };
            let (d, t) = refmap.get(norm_key(lab, norm))?;
            *ref_out = Some(RefInfo {
                identifier: normalize_label(lab).into_owned(),
                label: unescape_string(lab).into_owned(),
                reftype: if collapsed { "collapsed" } else { "full" },
            });
            return Some((d.as_str(), t.as_deref(), end));
        }
        return None;
    }
    // Shortcut reference: the link text itself is the label.
    let (d, t) = refmap.get(norm_key(text, norm))?;
    *ref_out = Some(RefInfo {
        identifier: normalize_label(text).into_owned(),
        label: unescape_string(text).into_owned(),
        reftype: "shortcut",
    });
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
/// `(raw label, raw destination, raw title)`. The caller normalizes the label
/// for the [`RefMap`] key (and, in AST mode, keeps the raw label for the
/// `definition` node).
pub fn take_ref_defs(text: &str) -> (usize, Vec<(String, String, Option<String>)>) {
    let bytes = text.as_bytes();
    let mut pos = 0;
    let mut defs = Vec::new();
    while let Some((end, label, dest, title, _, _)) = parse_ref_def(text, bytes, pos) {
        defs.push((label, dest, title));
        pos = end;
    }
    (pos, defs)
}

/// SPIKE (`ast`): the source byte span `(start, end)` of each leading reference
/// definition (start at `[`, end after the last significant char — no trailing
/// whitespace/newline), for the `definition` node's unist `position`.
#[cfg(feature = "ast")]
pub fn take_ref_def_spans(text: &str) -> Vec<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut pos = 0;
    let mut spans = Vec::new();
    while let Some((end, _, _, _, bracket, content_end)) = parse_ref_def(text, bytes, pos) {
        spans.push((bracket, content_end));
        pos = end;
    }
    spans
}

/// Returns `(end, label, dest, title, bracket_start, content_end)`. `bracket_start`
/// is the `[`; `content_end` is just past the last significant byte (title, or
/// dest when untitled) — both for AST `position`.
fn parse_ref_def(
    text: &str,
    bytes: &[u8],
    start: usize,
) -> Option<(usize, String, String, Option<String>, usize, usize)> {
    let mut j = start;
    let mut ind = 0;
    while ind < 3 && bytes.get(j) == Some(&b' ') {
        j += 1;
        ind += 1;
    }
    if bytes.get(j) != Some(&b'[') {
        return None;
    }
    let bracket = j;
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
            bracket,
            after_title,
        ));
    }
    // A trailing-junk title invalidates only the title, not the whole def.
    if title.is_some()
        && let Some(end) = ref_line_end(bytes, dj)
    {
        return Some((end, label.to_string(), dest.to_string(), None, bracket, dj));
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
// GFM strikethrough (`~`) is handled when a `~` delimiter is present, which
// only happens with the option on; `strike` is branch-predicted false otherwise.
fn process_emphasis(list: &mut List, stack: &mut Vec<StackItem>, start: usize) {
    let mut openers_bottom = [[-1isize; 3]; 3];
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
        let strike = Options::GFM && cch == b'~';
        let char_idx = if strike {
            2
        } else if cch == b'*' {
            0
        } else {
            1
        };
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
            // The "multiple of 3" rule is emphasis-only; GFM `~` matches by
            // equal run length instead.
            let odd_match =
                !strike && (ccan_open || ocan_close) && corig % 3 != 0 && (oorig + corig) % 3 == 0;
            let len_match = !strike || ocount == ccount;
            if och == cch && ocan_open && !odd_match && len_match {
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
                let (open_tag, close_tag, use_delims) = if strike {
                    // GFM strikethrough — equal-length match consumes the run.
                    ("<del>", "</del>", ocount)
                } else {
                    let strong = ocount >= 2 && ccount >= 2;
                    if strong {
                        ("<strong>", "</strong>", 2)
                    } else {
                        ("<em>", "</em>", 1)
                    }
                };
                let otag = list.splice_after(onode, Node::Tag(open_tag));
                let ctag = list.splice_before(cnode, Node::Tag(close_tag));
                #[cfg(not(feature = "ast"))]
                let _ = (otag, ctag);
                // SPIKE (`ast`): markers are 1 byte each. The opener consumes its
                // rightmost `use_delims` chars, the closer its leftmost; each tag
                // gets the consumed chars, and the run's remaining span shrinks
                // accordingly (so an unconsumed remainder, rendered as literal
                // text, is positioned correctly). Operate on the *current* run
                // bounds so repeated matches on the same run stay consistent.
                #[cfg(feature = "ast")]
                {
                    let ud = use_delims as u32;
                    let o_end = list.slots[onode].cspan.1;
                    list.slots[otag].cspan = (o_end - ud, o_end);
                    list.slots[onode].cspan.1 = o_end - ud;
                    let c_start = list.slots[cnode].cspan.0;
                    list.slots[ctag].cspan = (c_start, c_start + ud);
                    list.slots[cnode].cspan.0 = c_start + ud;
                }
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
        render_inline(
            s,
            &mut out,
            &RefMap::new(),
            &mut Scratch::new(),
            crate::options::Options::default(),
        );
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
        render_inline(
            "[foo]",
            &mut out,
            &map,
            &mut sc,
            crate::options::Options::default(),
        );
        assert_eq!(out, "<a href=\"/url\" title=\"t\">foo</a>");

        let mut out2 = String::new();
        render_inline(
            "[bar][foo]",
            &mut out2,
            &map,
            &mut sc,
            crate::options::Options::default(),
        );
        assert_eq!(out2, "<a href=\"/url\" title=\"t\">bar</a>");
    }
}
