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
    let html = crate::to_html(&md);
    let bytes = html.as_bytes();

    // Length-prefixed output so the host learns the size from one return value.
    let mut out = Vec::<u8>::with_capacity(4 + bytes.len());
    out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(bytes);
    let ret = out.as_mut_ptr();
    core::mem::forget(out);
    ret
}
