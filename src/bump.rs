//! A minimal zero-dependency bump arena that hands out `&'bump str`
//! references living as long as the arena itself.
//!
//! A single `to_html` materializes a flood of short-lived owned strings —
//! the typography rewrites, the de-prefixed blockquote/list bodies copied
//! out of their scratch parse, the re-serialized raw HTML — every one of
//! them freed together when the call returns. The system allocator pays a
//! `malloc`/`free` per string and the AST teardown runs a destructor per
//! node; profiling the gem's shipped (no-`unsafe`-feature) build put that
//! whole malloc + drop family at ~18 % of render time, second only to the
//! inline parser.
//!
//! [`Bump`] collapses it: each owned string is copied once into a chunk by
//! a pointer bump, the AST stores a plain `Cow::Borrowed(&'bump str)` (so a
//! node carries no owned heap and its `Drop` is a no-op), and the whole
//! arena is freed in a handful of `dealloc`s when `Bump` drops. Unlike the
//! `arena` feature's [`crate::arena::ScopedAlloc`] — a process-global
//! `#[global_allocator]` that intercepts *every* allocation in the scope
//! (and so must be carefully paused around anything that escapes, like the
//! gem's code-block recorder) — `Bump` is local: it holds only what the
//! parser explicitly hands it, so escaping allocations stay on the system
//! allocator with no ceremony, and the one `unsafe` block here is
//! Miri-checkable.
//!
//! # Soundness
//! Chunks are raw [`alloc`] regions, never a `Vec` whose buffer could move,
//! and the cursor advances through them with raw pointers — no `&mut` is
//! ever formed over a chunk, so a handed-out `&'bump str` is never
//! invalidated by a later allocation into the same chunk (writes only ever
//! touch the untouched tail). A chunk is freed only in [`Bump`]'s `Drop`,
//! after every borrow of it has ended.

#![allow(unsafe_code)]

use std::alloc::{self, Layout};
use std::cell::{Cell, RefCell};
use std::ptr;

/// First chunk size; subsequent chunks double (bounded by the request), so
/// a typical document settles into one or two chunks.
const FIRST_CHUNK: usize = 4096;

/// A live chunk: its base pointer and capacity, kept for deallocation.
#[derive(Debug)]
struct Chunk {
    ptr: *mut u8,
    cap: usize,
}

/// A bump-pointer arena handing out byte/string slices borrowed for its own
/// lifetime. Allocation is `&self` (interior mutability) so an `&Bump`
/// threaded through the parser can fill it while the AST borrows from it.
#[derive(Debug)]
pub(crate) struct Bump {
    /// Base of the current (last-allocated) chunk; null before the first
    /// allocation.
    cur_ptr: Cell<*mut u8>,
    /// Capacity of the current chunk.
    cur_cap: Cell<usize>,
    /// Bytes used in the current chunk.
    cur_len: Cell<usize>,
    /// Every chunk ever allocated, in order, for `Drop`. The current chunk
    /// is always the last entry. Touched only on the cold grow path and at
    /// drop, never aliasing a handed-out slice.
    chunks: RefCell<Vec<Chunk>>,
}

impl Bump {
    /// An empty arena. No allocation happens until the first `alloc_*`.
    pub(crate) fn new() -> Bump {
        Bump {
            cur_ptr: Cell::new(ptr::null_mut()),
            cur_cap: Cell::new(0),
            cur_len: Cell::new(0),
            chunks: RefCell::new(Vec::new()),
        }
    }

    /// An arena whose first chunk already holds ~`cap` bytes, so a render's
    /// worth of owned strings (typography rewrites, de-prefixed bodies) lands
    /// in one allocation instead of growing 4K→8K→16K→… through a chain of
    /// `malloc`s. Sized from `src.len()`; over-sizing is lazy virtual pages,
    /// so a generous estimate is free.
    pub(crate) fn with_capacity(cap: usize) -> Bump {
        let b = Bump::new();
        if cap > 0 {
            b.grow(cap);
        }
        b
    }

    /// Copy `s` into the arena, returning a slice valid for `&self`.
    #[inline]
    pub(crate) fn alloc_str(&self, s: &str) -> &str {
        // SAFETY: the bytes are copied verbatim from a `&str`, so the
        // returned region is still well-formed UTF-8.
        unsafe { std::str::from_utf8_unchecked(self.alloc_bytes(s.as_bytes())) }
    }

    /// Copy `src` into the arena, returning a slice valid for `&self`.
    #[inline]
    fn alloc_bytes(&self, src: &[u8]) -> &[u8] {
        let n = src.len();
        if n == 0 {
            return &[];
        }
        let mut len = self.cur_len.get();
        // Remaining = cap - len (cap >= len always holds), so this also
        // covers the null/empty initial chunk (remaining 0) with no risk of
        // `len + n` overflowing.
        if n > self.cur_cap.get() - len {
            self.grow(n);
            len = 0;
        }
        let base = self.cur_ptr.get();
        // SAFETY: `grow` guarantees `base` points at a chunk with at least
        // `len + n` capacity; the region `[base+len, base+len+n)` is owned
        // by this arena, untouched (the cursor only moves forward, never
        // revisiting handed-out bytes), and freed only in `Drop` after all
        // borrows end — so the returned slice is valid for `&self`.
        unsafe {
            let dst = base.add(len);
            ptr::copy_nonoverlapping(src.as_ptr(), dst, n);
            self.cur_len.set(len + n);
            std::slice::from_raw_parts(dst, n)
        }
    }

    /// Allocate a fresh chunk big enough for `need` bytes and make it
    /// current. Cold: hit once per chunk, not per allocation.
    #[cold]
    #[inline(never)]
    fn grow(&self, need: usize) {
        let cap = self
            .cur_cap
            .get()
            .max(FIRST_CHUNK)
            .max(need)
            .next_power_of_two();
        let layout = Layout::from_size_align(cap, 1).expect("bump chunk layout");
        // SAFETY: `cap` is non-zero (>= FIRST_CHUNK), align 1 is valid.
        let p = unsafe { alloc::alloc(layout) };
        if p.is_null() {
            alloc::handle_alloc_error(layout);
        }
        self.chunks.borrow_mut().push(Chunk { ptr: p, cap });
        self.cur_ptr.set(p);
        self.cur_cap.set(cap);
        self.cur_len.set(0);
    }
}

impl Drop for Bump {
    fn drop(&mut self) {
        for c in self.chunks.borrow().iter() {
            let layout = Layout::from_size_align(c.cap, 1).expect("bump chunk layout");
            // SAFETY: `c.ptr`/`c.cap` came from `alloc::alloc` with this
            // exact layout in `grow`, and every borrow handed out from this
            // chunk has ended (the arena is being dropped).
            unsafe { alloc::dealloc(c.ptr, layout) };
        }
    }
}

// A `Bump` is single-threaded by construction (Cell/RefCell); it is neither
// Send nor Sync, which is exactly right — the parser uses it on one thread.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alloc_str_roundtrips_and_is_stable() {
        let b = Bump::new();
        let a = b.alloc_str("alpha");
        let c = b.alloc_str("");
        let d = b.alloc_str("a longer string that may or may not share a chunk");
        // Earlier handles stay valid after later allocations.
        assert_eq!(a, "alpha");
        assert_eq!(c, "");
        assert_eq!(d, "a longer string that may or may not share a chunk");
    }

    #[test]
    fn spans_multiple_chunks() {
        let b = Bump::new();
        let mut handles = Vec::new();
        // Force several chunk growths and keep every handle live.
        for i in 0..2000 {
            let s = format!("item-{i:04}-{}", "x".repeat(i % 64));
            handles.push((b.alloc_str(&s), s));
        }
        for (got, want) in &handles {
            assert_eq!(*got, want.as_str());
        }
    }

    #[test]
    fn single_oversized_alloc() {
        let b = Bump::new();
        let big = "z".repeat(100_000);
        let got = b.alloc_str(&big);
        assert_eq!(got.len(), 100_000);
        assert_eq!(got, big);
        // A small alloc after the oversized one still works.
        assert_eq!(b.alloc_str("tail"), "tail");
    }

    #[test]
    fn with_capacity_holds_a_render_in_one_chunk() {
        let b = Bump::with_capacity(4000);
        // Many small allocs that together fit the pre-sized chunk should not
        // grow it (one chunk), yet every handle stays valid.
        let mut hs = Vec::new();
        for i in 0..200 {
            hs.push(b.alloc_str(&format!("s{i}")));
        }
        for (i, h) in hs.iter().enumerate() {
            assert_eq!(*h, format!("s{i}"));
        }
        assert_eq!(b.chunks.borrow().len(), 1, "pre-sized chunk should suffice");
        // A zero-capacity request is just an empty arena.
        let z = Bump::with_capacity(0);
        assert_eq!(z.alloc_str("x"), "x");
    }

    #[test]
    fn distinct_regions_do_not_overlap() {
        let b = Bump::new();
        let x = b.alloc_str("first");
        let y = b.alloc_str("second");
        assert_ne!(x.as_ptr(), y.as_ptr());
        assert_eq!(x, "first");
        assert_eq!(y, "second");
    }
}
