//! Shared parser for the remark-directive grammar (text / leaf / container
//! directives), opt-in via [`crate::Options::directives`]. A directive header is
//! `name`, then an optional `[label]`, then optional `{attributes}` — the part
//! that follows the leading colon(s). The same header parser drives all three
//! forms; only the colon count and block/inline framing differ.
//!
//! Attributes mirror micromark-extension-directive: `#id` sets `id` (last wins),
//! `.cls` accumulates into `class` (space-joined), and `key` / `key=value` /
//! `key="value"` set arbitrary pairs (last wins). Key order in the resulting
//! object is first-occurrence order — represented here as a `Vec<(key, value)>`.

/// A parsed directive header.
pub(crate) struct Header {
    /// `[name_start, name_end)` byte range of the name within the input slice.
    pub name_start: usize,
    pub name_end: usize,
    /// Content range of `[label]` (excluding the brackets), if present.
    pub label: Option<(usize, usize)>,
    /// Attributes in first-occurrence key order.
    pub attrs: Vec<(String, String)>,
    /// Total bytes consumed from the input (name + label + attrs).
    pub consumed: usize,
}

#[inline]
fn is_name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'-' || b == b'_'
}

/// Parse a directive header from `s`, which begins immediately after the leading
/// colon(s). Returns `None` when there is no valid name.
pub(crate) fn parse_header(s: &[u8]) -> Option<Header> {
    let n = s.len();
    let mut i = 0;
    while i < n && is_name_byte(s[i]) {
        i += 1;
    }
    if i == 0 {
        return None;
    }
    let (name_start, name_end) = (0, i);

    // Optional `[label]` — a balanced bracket span (backslash escapes ignored for
    // bracket matching, matching the common case).
    let mut label = None;
    if i < n
        && s[i] == b'['
        && let Some(end) = label_end(s, i)
    {
        label = Some((i + 1, end));
        i = end + 1;
    }

    // Optional `{attributes}`.
    let mut attrs = Vec::new();
    if i < n
        && s[i] == b'{'
        && let Some(end) = attr_end(s, i)
    {
        parse_attrs(&s[i + 1..end], &mut attrs);
        i = end + 1;
    }

    Some(Header {
        name_start,
        name_end,
        label,
        attrs,
        consumed: i,
    })
}

/// Index of the `]` matching the `[` at `open`, respecting nested brackets and
/// backslash escapes; `None` if unbalanced.
fn label_end(s: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut i = open;
    while i < s.len() {
        match s[i] {
            b'\\' => i += 1, // skip the escaped byte
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Index of the `}` matching the `{` at `open`, respecting quoted strings (a `}`
/// inside `"…"`/`'…'` does not close); `None` if unterminated.
fn attr_end(s: &[u8], open: usize) -> Option<usize> {
    let mut i = open + 1;
    let mut quote = 0u8;
    while i < s.len() {
        let b = s[i];
        if quote != 0 {
            if b == quote {
                quote = 0;
            }
        } else if b == b'"' || b == b'\'' {
            quote = b;
        } else if b == b'}' {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Insert `(key, value)`, merging with an existing key: `class` accumulates
/// (space-joined), every other key is replaced (last wins). New keys append.
fn set_attr(attrs: &mut Vec<(String, String)>, key: &str, value: String) {
    if let Some(slot) = attrs.iter_mut().find(|(k, _)| k == key) {
        if key == "class" {
            slot.1.push(' ');
            slot.1.push_str(&value);
        } else {
            slot.1 = value;
        }
    } else {
        attrs.push((key.to_string(), value));
    }
}

/// Tokenize the inside of `{…}` into attribute pairs.
fn parse_attrs(s: &[u8], attrs: &mut Vec<(String, String)>) {
    let n = s.len();
    let mut i = 0;
    while i < n {
        // Skip separators.
        if s[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        match s[i] {
            b'#' => {
                let start = i + 1;
                i = start;
                while i < n && !s[i].is_ascii_whitespace() {
                    i += 1;
                }
                set_attr(attrs, "id", str_of(&s[start..i]));
            }
            b'.' => {
                let start = i + 1;
                i = start;
                while i < n && !s[i].is_ascii_whitespace() {
                    i += 1;
                }
                set_attr(attrs, "class", str_of(&s[start..i]));
            }
            _ => {
                // key, key=value, or key="value"/'value'.
                let kstart = i;
                while i < n && s[i] != b'=' && !s[i].is_ascii_whitespace() {
                    i += 1;
                }
                let key = str_of(&s[kstart..i]);
                if key.is_empty() {
                    i += 1;
                    continue;
                }
                if i < n && s[i] == b'=' {
                    i += 1; // skip '='
                    let value = if i < n && (s[i] == b'"' || s[i] == b'\'') {
                        let q = s[i];
                        i += 1;
                        let vstart = i;
                        while i < n && s[i] != q {
                            i += 1;
                        }
                        let v = str_of(&s[vstart..i]);
                        if i < n {
                            i += 1; // closing quote
                        }
                        v
                    } else {
                        let vstart = i;
                        while i < n && !s[i].is_ascii_whitespace() {
                            i += 1;
                        }
                        str_of(&s[vstart..i])
                    };
                    set_attr(attrs, &key, value);
                } else {
                    set_attr(attrs, &key, String::new());
                }
            }
        }
    }
}

#[inline]
fn str_of(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}
