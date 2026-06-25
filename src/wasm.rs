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

/// Render `len` UTF-8 bytes at `ptr` to HTML. Returns a pointer to a buffer
/// `[u32 little-endian length][HTML bytes]`; free it with
/// [`sparkdown_free`]`(ret, 4 + length)`.
///
/// # Safety
/// `ptr` must point to `len` readable, initialized bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkdown_to_html(ptr: *const u8, len: usize) -> *mut u8 {
    let input = unsafe { core::slice::from_raw_parts(ptr, len) };
    let md = String::from_utf8_lossy(input);
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
    let md = String::from_utf8_lossy(input);
    box_html(crate::ast::to_mdast_json(&md).as_bytes())
}

/// Like [`sparkdown_to_html`] but applies extension options from a bitmask: bit
/// 0 strikethrough, 1 task lists, 2 autolinks, 3 tag filter, 4 tables, 5 hard
/// wraps, 6 diagram. A bit only takes effect if the matching Cargo feature was
/// compiled in. Built with the `gfm` feature.
///
/// # Safety
/// `ptr` must point to `len` readable, initialized bytes.
#[cfg(feature = "gfm")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparkdown_to_html_opts(ptr: *const u8, len: usize, flags: u32) -> *mut u8 {
    let input = unsafe { core::slice::from_raw_parts(ptr, len) };
    let md = String::from_utf8_lossy(input);
    let opts = crate::Options {
        strikethrough: flags & 1 != 0,
        tasklist: flags & 2 != 0,
        autolink: flags & 4 != 0,
        tagfilter: flags & 8 != 0,
        tables: flags & 16 != 0,
        hard_wraps: flags & 32 != 0,
        diagram: flags & 64 != 0,
    };
    RENDERER.with(|cell| {
        let mut r = cell.borrow_mut();
        r.set_options(opts);
        box_html(r.render(&md).as_bytes())
    })
}
