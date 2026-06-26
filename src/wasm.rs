//! WASI-free WebAssembly interface — raw, zero-dependency C-ABI exports for the
//! `wasm32-unknown-unknown` target. No `wasm-bindgen`, no WASI, no host imports:
//! any wasm host (browser, Cloudflare Workers, Deno, wasmtime, wazero, …) drives
//! it with four calls.
//!
//! Protocol:
//! 1. `p = sparkdown_alloc(len)` — reserve `len` bytes; write the UTF-8 markdown
//!    into linear memory at `p`.
//! 2. `out = sparkdown_to_html(p, len)` — render. `out` points to a buffer laid
//!    out as `[u32 little-endian length][HTML bytes]`.
//! 3. read the 4-byte length, then that many HTML bytes from `out + 4`.
//! 4. `sparkdown_free(p, len)` and `sparkdown_free(out, 4 + html_len)`.
//!
//! The `src` pointer + length convention keeps the ABI tiny and host-agnostic;
//! the JS glue in the npm package wraps it as `toHtml(markdown) -> string`.

/// Reserve `len` bytes in linear memory and return the pointer. The host writes
/// the input there before calling [`sparkdown_to_html`]. Free with
/// [`sparkdown_free`]`(ptr, len)`.
#[unsafe(no_mangle)]
pub extern "C" fn sparkdown_alloc(len: usize) -> *mut u8 {
    // `Vec::with_capacity` records capacity == len, so the matching free is exact.
    let mut buf = Vec::<u8>::with_capacity(len);
    let ptr = buf.as_mut_ptr();
    core::mem::forget(buf);
    ptr
}

/// Free a `len`-byte block previously returned by [`sparkdown_alloc`] (pass the
/// original `len`) or [`sparkdown_to_html`] (pass `4 + html_len`).
///
/// # Safety
/// `ptr` must be a pointer returned by one of those functions with the matching
/// `len`, freed at most once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkdown_free(ptr: *mut u8, len: usize) {
    if !ptr.is_null() {
        drop(unsafe { Vec::from_raw_parts(ptr, 0, len) });
    }
}

thread_local! {
    // A persistent context: its buffers stay warm across calls, so a long-lived
    // wasm instance rendering many documents avoids re-allocating them on
    // dlmalloc each time.
    static RENDERER: core::cell::RefCell<crate::Renderer> =
        core::cell::RefCell::new(crate::Renderer::new());
}

/// Decode the extension `flags` bitmask shared by every `*_opts` export into an
/// [`crate::Options`]. Bit layout (LSB first): 0 strikethrough, 1 task lists,
/// 2 autolinks, 3 tag filter, 4 tables, 5 hard wraps, 6 diagram, 7 heading ids,
/// 8 frontmatter, 9 footnotes, 10 emoji, 11 external links, 12 definition lists,
/// 13 directives — 14 flags total. A bit takes effect only if its Cargo feature
/// was compiled in.
fn opts_from_flags(flags: u32) -> crate::Options {
    crate::Options {
        strikethrough: flags & 1 != 0,
        tasklist: flags & 2 != 0,
        autolink: flags & 4 != 0,
        tagfilter: flags & 8 != 0,
        tables: flags & 16 != 0,
        hard_wraps: flags & 32 != 0,
        diagram: flags & 64 != 0,
        heading_ids: flags & 128 != 0,
        frontmatter: flags & 256 != 0,
        footnotes: flags & 512 != 0,
        emoji: flags & 1024 != 0,
        external_links: flags & 2048 != 0,
        deflist: flags & 4096 != 0,
        directives: flags & 8192 != 0,
    }
}

/// Box `bytes` as a freshly-allocated `[u32 little-endian length][bytes]` buffer
/// and leak it; the host reads the length then the bytes, and frees with
/// [`sparkdown_free`]`(ret, 4 + length)`.
fn box_html(bytes: &[u8]) -> *mut u8 {
    let mut out = Vec::<u8>::with_capacity(4 + bytes.len());
    out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(bytes);
    let ret = out.as_mut_ptr();
    core::mem::forget(out);
    ret
}

/// Decode `input` as UTF-8 for parsing. `str::from_utf8`'s validation uses the
/// word-at-a-time ASCII fast path — measurably cheaper than `String::from_utf8_lossy`'s
/// `Utf8Chunks` iterator (~12% of parse time on the profile). Invalid UTF-8 (rare — the
/// JS glue always sends `TextEncoder` output) falls back to lossy U+FFFD replacement so
/// the boundary stays total.
#[inline]
fn input_str(input: &[u8]) -> std::borrow::Cow<'_, str> {
    match core::str::from_utf8(input) {
        Ok(s) => std::borrow::Cow::Borrowed(s),
        Err(_) => String::from_utf8_lossy(input),
    }
}

/// Render `len` UTF-8 bytes at `ptr` to HTML. Returns a pointer to a buffer
/// `[u32 little-endian length][HTML bytes]`; free it with
/// [`sparkdown_free`]`(ret, 4 + length)`.
///
/// # Safety
/// `ptr` must point to `len` readable, initialized bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkdown_to_html(ptr: *const u8, len: usize) -> *mut u8 {
    let input = unsafe { core::slice::from_raw_parts(ptr, len) };
    let md = input_str(input);
    RENDERER.with(|cell| box_html(cell.borrow_mut().render(&md).as_bytes()))
}

/// SPIKE (`ast` feature): parse `len` UTF-8 bytes at `ptr` and return the mdast
/// as JSON, in the same `[u32 little-endian length][bytes]` framing as
/// [`sparkdown_to_html`]. This is the payload the wasm→JS boundary spike moves
/// across; the host does `JSON.parse` on the bytes to get a remark-shaped tree.
///
/// # Safety
/// `ptr` must point to `len` readable, initialized bytes.
#[cfg(feature = "ast")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkdown_to_mdast_json(ptr: *const u8, len: usize) -> *mut u8 {
    let input = unsafe { core::slice::from_raw_parts(ptr, len) };
    let md = input_str(input);
    box_html(crate::ast::to_mdast_json(&md).as_bytes())
}

/// `ast`: parse → build the mdast tree → render it back to HTML, all in wasm —
/// the Rust `mdast → html` half (see [`crate::ast::render_mdast`]), byte-identical
/// to `mdast-util-to-hast` + `hast-util-to-html` on all 652 CommonMark examples.
/// Faster than satteri's native pipeline; keeps the unified-pipeline render off the
/// JS `mdast→hast→html` tail. `flags` is the [`sparkdown_to_html_opts`] bitmask.
///
/// # Safety
/// `ptr` must point to `len` readable, initialized bytes.
#[cfg(feature = "ast")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkdown_to_html_via_mdast_opts(
    ptr: *const u8,
    len: usize,
    flags: u32,
) -> *mut u8 {
    let input = unsafe { core::slice::from_raw_parts(ptr, len) };
    let md = input_str(input);
    let tree = crate::ast::to_mdast_opts_nopos(&md, opts_from_flags(flags));
    let mut out = String::with_capacity(md.len() + md.len() / 8 + 64);
    crate::ast::render_mdast_into(&tree, &mut out);
    box_html(out.as_bytes())
}

/// SPIKE (`ast` feature, route A): parse `len` UTF-8 bytes at `ptr` and return the
/// mdast in the compact **binary wire format** (see [`crate::ast::to_mdast_wire`]),
/// in the same `[u32 little-endian length][bytes]` framing as the others. The host
/// reads the bytes directly out of linear memory into plain JS objects — no JSON.
///
/// # Safety
/// `ptr` must point to `len` readable, initialized bytes.
#[cfg(feature = "ast")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkdown_to_mdast_wire(ptr: *const u8, len: usize) -> *mut u8 {
    let input = unsafe { core::slice::from_raw_parts(ptr, len) };
    let md = input_str(input);
    box_html(&crate::ast::to_mdast_wire(&md))
}

/// Like [`sparkdown_to_mdast_json`] but applies opt-in grammar extensions from a
/// `flags` bitmask (same layout as [`sparkdown_to_html_opts`]). Returns JSON in
/// the standard `[u32 length][bytes]` framing. The JSON path carries full inline
/// children for every node (e.g. a `textDirective`'s `[label]`), so it is the
/// path the directive alignment gate checks against remark-directive.
///
/// # Safety
/// `ptr` must point to `len` readable, initialized bytes.
#[cfg(feature = "ast")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkdown_to_mdast_json_opts(
    ptr: *const u8,
    len: usize,
    flags: u32,
) -> *mut u8 {
    let input = unsafe { core::slice::from_raw_parts(ptr, len) };
    let md = input_str(input);
    let opts = opts_from_flags(flags);
    box_html(crate::ast::to_mdast_json_opts(&md, opts).as_bytes())
}

/// Like [`sparkdown_to_mdast_wire`] but applies opt-in grammar extensions from a
/// `flags` bitmask (same bit layout as [`sparkdown_to_html_opts`]; bit 8 =
/// frontmatter). Lets the JS boundary request a frontmatter-aware mdast tree.
///
/// # Safety
/// `ptr` must point to `len` readable, initialized bytes.
#[cfg(feature = "ast")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkdown_to_mdast_wire_opts(
    ptr: *const u8,
    len: usize,
    flags: u32,
) -> *mut u8 {
    let input = unsafe { core::slice::from_raw_parts(ptr, len) };
    let md = input_str(input);
    let opts = opts_from_flags(flags);
    box_html(&crate::ast::to_mdast_wire_opts(&md, opts))
}

/// Like [`sparkdown_to_mdast_wire_opts`] but emits the **no-position** wire
/// (see [`crate::ast::to_mdast_wire_nopos_opts`]): every node's `6×u32` start/end
/// point is omitted and the heavy UTF-16 position table + source copy are never
/// built. For consumers (e.g. remark plugins) that never read `position`; ships
/// ~100 KB less and parses faster. Same `flags` bitmask as
/// [`sparkdown_to_mdast_wire_opts`]. The non-position payload bytes are identical
/// to the position-on wire — the JS reader must be told to skip the point reads.
///
/// # Safety
/// `ptr` must point to `len` readable, initialized bytes.
#[cfg(feature = "ast")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkdown_to_mdast_wire_nopos_opts(
    ptr: *const u8,
    len: usize,
    flags: u32,
) -> *mut u8 {
    let input = unsafe { core::slice::from_raw_parts(ptr, len) };
    let md = input_str(input);
    let opts = opts_from_flags(flags);
    box_html(&crate::ast::to_mdast_wire_nopos_opts(&md, opts))
}

/// Like [`sparkdown_to_mdast_wire_nopos_opts`] but emits the **string-pooled**
/// no-position wire (see [`crate::ast::to_mdast_wire_fast_opts`]): the structure
/// holds only `u32` UTF-16 string lengths and all UTF-8 string bytes are packed
/// into one contiguous pool, so a JS reader decodes the pool with a single
/// `TextDecoder.decode` and slices substrings. Layout is
/// `[u32 poolStart][structure][pool]`. Same `flags` bitmask as
/// [`sparkdown_to_mdast_wire_opts`].
///
/// # Safety
/// `ptr` must point to `len` readable, initialized bytes.
#[cfg(feature = "ast")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkdown_to_mdast_wire_fast_opts(
    ptr: *const u8,
    len: usize,
    flags: u32,
) -> *mut u8 {
    let input = unsafe { core::slice::from_raw_parts(ptr, len) };
    let md = input_str(input);
    let opts = opts_from_flags(flags);
    box_html(&crate::ast::to_mdast_wire_fast_opts(&md, opts))
}

/// Like [`sparkdown_to_html`] but applies extension options from a bitmask: bit
/// 0 strikethrough, 1 task lists, 2 autolinks, 3 tag filter, 4 tables, 5 hard
/// wraps, 6 diagram, 7 heading ids (built-in slug transform), 8 frontmatter
/// (YAML `---` / TOML `+++`), 9 footnotes (GFM), 10 emoji, 11 external links,
/// 12 definition lists, 13 directives. A bit only takes
/// effect if the matching Cargo feature was compiled in. Built with the `gfm`
/// feature.
///
/// # Safety
/// `ptr` must point to `len` readable, initialized bytes.
#[cfg(feature = "gfm")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkdown_to_html_opts(ptr: *const u8, len: usize, flags: u32) -> *mut u8 {
    let input = unsafe { core::slice::from_raw_parts(ptr, len) };
    let md = input_str(input);
    let opts = opts_from_flags(flags);
    RENDERER.with(|cell| {
        let mut r = cell.borrow_mut();
        r.set_options(opts);
        box_html(r.render(&md).as_bytes())
    })
}
