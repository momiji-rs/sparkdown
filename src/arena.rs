//! Optional scoped bump allocator (behind the `arena` feature).
//!
//! A single `to_html` is a flood of short-lived allocations (the
//! `Block`/`Span` tree) freed all at once, so when an embedder installs
//! [`ScopedAlloc`] as its `#[global_allocator]`, every allocation inside
//! `to_html`'s [`Scope`] becomes a pointer bump from a per-thread arena,
//! and the arena resets when the call finishes. Outside a scope (or when
//! `ScopedAlloc` is not installed) allocations forward to the system
//! allocator, so the scope primitives are inert and harmless.
//!
//! Ported verbatim (native subset) from rust-sass `src/arena.rs`; that
//! version carries the full Miri/ASan safety story. This is the library's
//! one `unsafe` module and only compiles under `--features arena`.

#![allow(unsafe_code)]
// pause/resume are wired up for the gem's Recorder-escape boundary later;
// keep them tested now even though to_html doesn't call them yet.
#![allow(dead_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

// ---- process-global arena-region registry (for dealloc routing) ----
const MAX_ARENAS: usize = 128;

struct Region {
    base: AtomicUsize,
    end: AtomicUsize,
}
#[allow(clippy::declare_interior_mutable_const)]
const ZERO_REGION: Region = Region {
    base: AtomicUsize::new(0),
    end: AtomicUsize::new(0),
};
static REGION_SLOTS: AtomicUsize = AtomicUsize::new(0);
static REGIONS: [Region; MAX_ARENAS] = [ZERO_REGION; MAX_ARENAS];

fn register_region(base: usize, end: usize) -> bool {
    let idx = REGION_SLOTS.fetch_add(1, Ordering::Relaxed);
    if idx >= MAX_ARENAS {
        return false;
    }
    REGIONS[idx].base.store(base, Ordering::Relaxed);
    REGIONS[idx].end.store(end, Ordering::Relaxed);
    true
}

#[inline]
fn in_any_arena(p: usize) -> bool {
    let n = REGION_SLOTS.load(Ordering::Relaxed).min(MAX_ARENAS);
    for r in &REGIONS[..n] {
        let base = r.base.load(Ordering::Relaxed);
        if base != 0 && p >= base && p < r.end.load(Ordering::Relaxed) {
            return true;
        }
    }
    false
}

/// Pure bump arithmetic: align `cur` up, add `size`, check it fits.
fn bump_compute(cur: usize, align: usize, size: usize, end: usize) -> Option<(usize, usize)> {
    let aligned = cur.checked_add(align - 1)? & !(align - 1);
    let next = aligned.checked_add(size)?;
    (next <= end).then_some((aligned, next))
}

const ARENA_SIZE: usize = 2 * 1024 * 1024 * 1024; // 2 GiB virtual (native)

struct ThreadState {
    base: Cell<*mut u8>,
    end: Cell<usize>,
    cursor: Cell<usize>,
    depth: Cell<u32>,
    reserve_failed: Cell<bool>,
}

impl ThreadState {
    const fn new() -> ThreadState {
        ThreadState {
            base: Cell::new(std::ptr::null_mut()),
            end: Cell::new(0),
            cursor: Cell::new(0),
            depth: Cell::new(0),
            reserve_failed: Cell::new(false),
        }
    }

    #[cold]
    fn reserve(&self) -> bool {
        let Ok(layout) = Layout::from_size_align(ARENA_SIZE, 4096) else {
            return false;
        };
        // SAFETY: non-zero size, 4096 is a valid power-of-two alignment.
        let p = unsafe { System.alloc(layout) };
        if p.is_null() {
            return false;
        }
        if !register_region(p as usize, p as usize + ARENA_SIZE) {
            // SAFETY: p came from System.alloc with this same layout.
            unsafe { System.dealloc(p, layout) };
            return false;
        }
        self.base.set(p);
        self.end.set(p as usize + ARENA_SIZE);
        self.cursor.set(p as usize);
        true
    }
}

thread_local! {
    // const init: no lazy alloc, POD (no Drop) so accessing it from
    // inside the global allocator can't re-enter it.
    static TL: ThreadState = const { ThreadState::new() };
}

/// A scoped bump global allocator. Install it in the embedding binary or
/// cdylib (only meaningful with the `arena` feature):
///
/// ```ignore
/// #[global_allocator]
/// static ALLOC: rostdown::ScopedAlloc = rostdown::ScopedAlloc;
/// ```
///
/// Safe to install even if `to_html` is never called: with no active
/// scope every request goes straight to the system allocator.
pub struct ScopedAlloc;

unsafe impl GlobalAlloc for ScopedAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        TL.with(|tl| {
            if tl.depth.get() == 0 {
                return unsafe { System.alloc(layout) };
            }
            if tl.base.get().is_null() {
                if tl.reserve_failed.get() {
                    return unsafe { System.alloc(layout) };
                }
                if !tl.reserve() {
                    tl.reserve_failed.set(true);
                    return unsafe { System.alloc(layout) };
                }
            }
            match bump_compute(tl.cursor.get(), layout.align(), layout.size(), tl.end.get()) {
                Some((aligned, next)) => {
                    tl.cursor.set(next);
                    let base = tl.base.get();
                    // SAFETY: bump_compute keeps base <= aligned and the
                    // end in-bounds, so the offset is valid.
                    unsafe { base.add(aligned - base as usize) }
                }
                None => unsafe { System.alloc(layout) },
            }
        })
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if !in_any_arena(ptr as usize) {
            // SAFETY: not from any arena → it came from System.
            unsafe { System.dealloc(ptr, layout) };
        }
        // in-arena: no-op (reclaimed wholesale on reset)
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // Tail-grow in place when `ptr` is the most recent arena bump.
        let resized = TL.with(|tl| {
            if tl.depth.get() == 0 {
                return false;
            }
            let base = tl.base.get();
            if base.is_null() {
                return false;
            }
            let addr = ptr as usize;
            if addr < base as usize || addr + layout.size() != tl.cursor.get() {
                return false;
            }
            match addr.checked_add(new_size) {
                Some(new_end) if new_end <= tl.end.get() => {
                    tl.cursor.set(new_end);
                    true
                }
                _ => false,
            }
        });
        if resized {
            return ptr;
        }
        // SAFETY: same contract as the default realloc.
        unsafe {
            let new_layout = Layout::from_size_align_unchecked(new_size, layout.align());
            let new_ptr = self.alloc(new_layout);
            if !new_ptr.is_null() {
                core::ptr::copy_nonoverlapping(ptr, new_ptr, layout.size().min(new_size));
                self.dealloc(ptr, layout);
            }
            new_ptr
        }
    }
}

/// RAII scope marker. On `drop` (panic / early-return path) it leaves the
/// scope and, if outermost, resets the arena. `to_html`'s success path
/// finishes manually (copy the result out, then [`reset`]) and
/// `mem::forget`s the guard.
pub struct Scope;

impl Scope {
    pub fn enter() -> Scope {
        TL.with(|tl| tl.depth.set(tl.depth.get() + 1));
        Scope
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        if leave_no_reset() {
            reset();
        }
    }
}

/// Leave the current scope WITHOUT resetting; returns whether this was the
/// outermost scope (the only one allowed to reset).
pub fn leave_no_reset() -> bool {
    TL.with(|tl| {
        let d = tl.depth.get().saturating_sub(1);
        tl.depth.set(d);
        d == 0
    })
}

/// Reset the arena to empty (only when no scope is active).
pub fn reset() {
    TL.with(|tl| {
        if tl.depth.get() == 0 {
            tl.cursor.set(tl.base.get() as usize);
        }
    });
}

/// Suspend the scope (allocations route to System) around a callback
/// whose allocations may outlive the arena — e.g. data that escapes
/// `to_html` via a recording highlighter. Returns the saved depth.
pub fn pause() -> u32 {
    TL.with(|tl| {
        let d = tl.depth.get();
        tl.depth.set(0);
        d
    })
}

/// Restore the depth saved by [`pause`].
pub fn resume(saved: u32) {
    TL.with(|tl| tl.depth.set(saved));
}

// ===================================================================
// Tests. The pure `bump_compute` and the Drop-ing `Arena` twin run
// under `rustup run nightly cargo miri test` for UB detection; the
// `ScopedAlloc` routing tests are #[cfg_attr(miri, ignore)] because
// Miri does not execute a #[global_allocator] and the thread-local
// backing intentionally leaks. Ported from rust-sass src/arena.rs.
// ===================================================================
#[cfg(test)]
struct Arena {
    base: *mut u8,
    size: usize,
    end: usize,
    cursor: Cell<usize>,
}

#[cfg(test)]
impl Arena {
    fn with_system_backing(size: usize) -> Option<Arena> {
        let layout = Layout::from_size_align(size, 4096).ok()?;
        // SAFETY: non-zero size, valid align.
        let base = unsafe { System.alloc(layout) };
        if base.is_null() {
            return None;
        }
        Some(Arena {
            base,
            size,
            end: base as usize + size,
            cursor: Cell::new(base as usize),
        })
    }

    fn alloc(&self, layout: Layout) -> Option<*mut u8> {
        let (aligned, next) =
            bump_compute(self.cursor.get(), layout.align(), layout.size(), self.end)?;
        self.cursor.set(next);
        // SAFETY: in-bounds offset (see bump_compute).
        Some(unsafe { self.base.add(aligned - self.base as usize) })
    }

    fn used(&self) -> usize {
        self.cursor.get() - self.base as usize
    }

    fn contains(&self, ptr: *mut u8) -> bool {
        let p = ptr as usize;
        p >= self.base as usize && p < self.end
    }

    fn realloc(&self, ptr: *mut u8, old: Layout, new_size: usize) -> Option<*mut u8> {
        let addr = ptr as usize;
        if addr >= self.base as usize && addr + old.size() == self.cursor.get() {
            let new_end = addr.checked_add(new_size)?;
            if new_end <= self.end {
                self.cursor.set(new_end);
                return Some(ptr);
            }
        }
        let np = self.alloc(Layout::from_size_align(new_size, old.align()).ok()?)?;
        // SAFETY: np is a fresh, non-overlapping allocation of >= copy length.
        unsafe { core::ptr::copy_nonoverlapping(ptr, np, old.size().min(new_size)) };
        Some(np)
    }

    fn reset(&self) {
        self.cursor.set(self.base as usize);
    }
}

#[cfg(test)]
impl Drop for Arena {
    fn drop(&mut self) {
        if let Ok(layout) = Layout::from_size_align(self.size, 4096) {
            // SAFETY: base came from System.alloc with this layout.
            unsafe { System.dealloc(self.base, layout) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(size: usize, align: usize) -> Layout {
        Layout::from_size_align(size, align).unwrap()
    }

    // ---- pure bump_compute (also exercised by Miri) ----

    #[test]
    fn compute_aligns_up() {
        assert_eq!(bump_compute(10, 8, 4, 1000), Some((16, 20)));
        assert_eq!(bump_compute(16, 8, 8, 1000), Some((16, 24)));
        assert_eq!(bump_compute(7, 1, 3, 1000), Some((7, 10)));
    }

    #[test]
    fn compute_every_power_of_two_alignment() {
        for align in [1usize, 2, 4, 8, 16, 32, 64, 128, 256, 4096] {
            let (aligned, next) = bump_compute(1, align, 64, usize::MAX).unwrap();
            assert_eq!(aligned % align, 0, "align {align}");
            assert_eq!(next, aligned + 64);
        }
    }

    #[test]
    fn compute_boundary_and_overflow() {
        assert_eq!(bump_compute(8, 8, 0, 100), Some((8, 8))); // zero-size
        assert_eq!(bump_compute(0, 1, 100, 100), Some((0, 100))); // exact fit
        assert_eq!(bump_compute(0, 1, 101, 100), None); // one past
        assert_eq!(bump_compute(90, 8, 20, 100), None); // align+size overflow end
        assert_eq!(bump_compute(usize::MAX, 8, 0, usize::MAX), None); // align overflow
        assert_eq!(bump_compute(usize::MAX - 3, 1, 10, usize::MAX), None); // size overflow
    }

    // ---- standalone Arena (run under Miri for UB) ----

    #[test]
    fn arena_alloc_aligned_writable_in_bounds_nonoverlapping() {
        let a = Arena::with_system_backing(64 * 1024).unwrap();
        let mut prev_end = a.base as usize;
        for align in [1usize, 2, 4, 8, 16, 64, 256] {
            let p = a.alloc(layout(128, align)).unwrap();
            assert_eq!(p as usize % align, 0, "align {align}");
            assert!(a.contains(p));
            assert!(p as usize >= prev_end, "no overlap");
            prev_end = p as usize + 128;
            // SAFETY: p is a live 128-byte allocation.
            unsafe {
                std::ptr::write_bytes(p, 0xAB, 128);
                assert_eq!(*p, 0xAB);
                assert_eq!(*p.add(127), 0xAB);
            }
        }
    }

    #[test]
    fn arena_full_returns_none() {
        let a = Arena::with_system_backing(4096).unwrap();
        assert!(a.alloc(layout(8192, 8)).is_none());
        assert!(a.alloc(layout(2048, 8)).is_some());
        assert!(a.alloc(layout(2048, 8)).is_some());
        assert!(a.alloc(layout(1, 1)).is_none());
    }

    #[test]
    fn arena_reset_reuses_region() {
        let a = Arena::with_system_backing(64 * 1024).unwrap();
        let p1 = a.alloc(layout(1000, 8)).unwrap();
        assert_eq!(a.used(), 1000);
        a.reset();
        assert_eq!(a.used(), 0);
        let p2 = a.alloc(layout(1000, 8)).unwrap();
        assert_eq!(p1, p2, "reset hands back the same region");
        // SAFETY: p2 is a live 1000-byte allocation.
        unsafe { std::ptr::write_bytes(p2, 0xCD, 1000) };
    }

    #[test]
    fn arena_realloc_extends_tail_else_copies() {
        let a = Arena::with_system_backing(64 * 1024).unwrap();
        let p = a.alloc(layout(8, 8)).unwrap();
        // SAFETY: p is a live 8-byte allocation.
        unsafe { std::ptr::write_bytes(p, 0xCD, 8) };
        let used = a.used();
        let p2 = a.realloc(p, layout(8, 8), 16).unwrap();
        assert_eq!(p, p2, "tail realloc grows in place");
        assert_eq!(a.used(), used + 8, "only the +8 delta is consumed");
        // SAFETY: p2 still points at the (now larger) live block.
        unsafe { assert_eq!(*p2, 0xCD, "data preserved in place") };
        let _q = a.alloc(layout(8, 8)).unwrap(); // p2 no longer the tail
        let p3 = a.realloc(p2, layout(16, 8), 32).unwrap();
        assert_ne!(p2, p3, "non-tail realloc copies to a fresh block");
        // SAFETY: p3 is the fresh block holding the copied bytes.
        unsafe { assert_eq!(*p3, 0xCD, "data copied to the new block") };
    }

    // ---- ScopedAlloc routing (NOT under Miri: no #[global_allocator]
    // runs there and the thread-local backing intentionally leaks) ----

    fn in_arena(p: *mut u8) -> bool {
        TL.with(|tl| {
            let b = tl.base.get() as usize;
            b != 0 && (p as usize) >= b && (p as usize) < tl.end.get()
        })
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn scoped_routes_to_system_when_inactive() {
        let l = layout(64, 8);
        // SAFETY: round-trips a System allocation; depth 0 → System.
        let p = unsafe { ScopedAlloc.alloc(l) };
        assert!(!p.is_null());
        assert!(!in_arena(p));
        unsafe { ScopedAlloc.dealloc(p, l) };
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn scoped_bumps_inside_scope_and_resets() {
        let l = layout(128, 16);
        let scope = Scope::enter();
        // SAFETY: in-scope allocations come from the arena.
        let p1 = unsafe { ScopedAlloc.alloc(l) };
        let p2 = unsafe { ScopedAlloc.alloc(l) };
        assert!(in_arena(p1) && in_arena(p2), "in-scope allocs are arena");
        assert!(p2 as usize >= p1 as usize + 128, "no overlap");
        assert_eq!(p1 as usize % 16, 0);
        // dealloc of an in-arena pointer is a no-op (must not free/crash).
        unsafe { ScopedAlloc.dealloc(p1, l) };
        assert!(leave_no_reset(), "outermost");
        reset();
        // After reset the region is reused.
        let scope2 = Scope::enter();
        let p3 = unsafe { ScopedAlloc.alloc(l) };
        assert_eq!(p3, p1, "reset hands back the same region");
        let _ = leave_no_reset();
        reset();
        std::mem::forget(scope2);
        std::mem::forget(scope);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn pause_routes_to_system_then_resumes() {
        let l = layout(64, 8);
        let scope = Scope::enter();
        let saved = pause(); // depth → 0
        // SAFETY: paused scope → System.
        let p_sys = unsafe { ScopedAlloc.alloc(l) };
        assert!(!in_arena(p_sys), "paused scope routes to System");
        unsafe { ScopedAlloc.dealloc(p_sys, l) };
        resume(saved);
        let p_arena = unsafe { ScopedAlloc.alloc(l) };
        assert!(in_arena(p_arena), "resumed scope bumps from the arena again");
        let _ = leave_no_reset();
        reset();
        std::mem::forget(scope);
    }
}
