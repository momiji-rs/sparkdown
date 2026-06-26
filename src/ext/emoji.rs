//! Emoji shortcode extension (`:smile:` → 😄), matching remark-gemoji.
//!
//! A `:name:` whose `name` (case-sensitively) is a known gemoji shortcode is
//! replaced by its Unicode emoji; anything else stays literal. The lookup runs
//! only in inline *text* (the inline parser handles code spans separately, so
//! `` `:smile:` `` is untouched), mirroring remark-gemoji's text-node transform.

#[path = "emoji_data.rs"]
mod emoji_data;

use emoji_data::EMOJI;

/// At `bytes[i]` (a `:`), try to match `:[-+\w]+:` whose inner name is a known
/// gemoji shortcode. Returns `(emoji, end)` where `end` is the byte index just
/// past the closing `:`. The scan charset mirrors remark-gemoji's `[-+\w]+`
/// (so `:SMILE:` is scanned but, being absent from the lowercase table, stays
/// literal — case-sensitive, exactly like remark-gemoji).
pub(crate) fn lookup(bytes: &[u8], i: usize) -> Option<(&'static str, usize)> {
    let start = i + 1;
    let mut j = start;
    while j < bytes.len()
        && matches!(bytes[j], b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'+' | b'-')
    {
        j += 1;
    }
    if j == start || bytes.get(j) != Some(&b':') {
        return None;
    }
    let name = std::str::from_utf8(&bytes[start..j]).ok()?;
    let idx = EMOJI.binary_search_by(|&(k, _)| k.cmp(name)).ok()?;
    Some((EMOJI[idx].1, j + 1))
}
