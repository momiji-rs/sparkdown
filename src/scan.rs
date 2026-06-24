//! SWAR (SIMD-Within-A-Register) byte search — zero-dep, no `unsafe`,
//! portable. Processes `size_of::<usize>()` bytes per iteration using the
//! classic "has-zero-byte" bit trick, ~4-8× a scalar loop without pulling
//! in the `memchr` crate or platform intrinsics. Used for the hot
//! escape/scan paths where the target bytes are rare in the haystack
//! (prose with occasional `&`/`<`/`>`).

const W: usize = std::mem::size_of::<usize>();
/// `0x0101…01` — one in every byte lane.
const LO: usize = usize::from_ne_bytes([0x01; W]);
/// `0x8080…80` — the high bit of every byte lane.
const HI: usize = usize::from_ne_bytes([0x80; W]);

/// `b` replicated into every byte lane (`0x0101…01 * b`).
#[inline]
fn broadcast(b: u8) -> usize {
    LO.wrapping_mul(b as usize)
}

/// A word whose byte lanes hold `0x80` exactly where `w`'s lane equals
/// the byte that `bcast` broadcasts. Borrows can plant a spurious high
/// bit in a *higher* lane, but never below a true match — so the lowest
/// set high bit is always a genuine hit (see [`first_hit`]).
#[inline]
fn zero_lanes(w: usize, bcast: usize) -> usize {
    let x = w ^ bcast;
    x.wrapping_sub(LO) & !x & HI
}

/// Byte index of the first set lane in `mask`, read little-endian so lane
/// 0 is the lowest source byte regardless of host endianness.
#[inline]
fn first_hit(mask: usize) -> usize {
    (mask.trailing_zeros() as usize) / 8
}

/// Index of the first byte in `hay` equal to `a` — a dependency-free
/// `memchr` (single byte; used for the `\n` line split).
#[inline]
pub(crate) fn memchr1(hay: &[u8], a: u8) -> Option<usize> {
    let ba = broadcast(a);
    let mut i = 0;
    while i + W <= hay.len() {
        let w = usize::from_le_bytes(hay[i..i + W].try_into().unwrap());
        let m = zero_lanes(w, ba);
        if m != 0 {
            return Some(i + first_hit(m));
        }
        i += W;
    }
    while i < hay.len() {
        if hay[i] == a {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Index of the first byte in `hay` equal to `a`, `b`, or `c` — a
/// dependency-free `memchr3`.
#[inline]
pub(crate) fn memchr3(hay: &[u8], a: u8, b: u8, c: u8) -> Option<usize> {
    let (ba, bb, bc) = (broadcast(a), broadcast(b), broadcast(c));
    let mut i = 0;
    while i + W <= hay.len() {
        // from_le_bytes: fixes lane order so first_hit is host-agnostic.
        let w = usize::from_le_bytes(hay[i..i + W].try_into().unwrap());
        let m = zero_lanes(w, ba) | zero_lanes(w, bb) | zero_lanes(w, bc);
        if m != 0 {
            return Some(i + first_hit(m));
        }
        i += W;
    }
    while i < hay.len() {
        let x = hay[i];
        if x == a || x == b || x == c {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Index of the first HTML-text special (`&`, `<`, `>`, `"`) in `hay` —
/// SIMD-accelerated (NEON on aarch64, SSE2 on x86_64; both baseline, so no
/// runtime detection), with the SWAR [`memchr4`] as the portable fallback.
/// Used by the hot escape loop.
#[inline]
pub(crate) fn find_escape(hay: &[u8]) -> Option<usize> {
    #[cfg(target_arch = "aarch64")]
    {
        // SAFETY: NEON is baseline on aarch64; reads are bounded by `i + 16 <= len`.
        unsafe { find_escape_neon(hay) }
    }
    #[cfg(target_arch = "x86_64")]
    {
        // SAFETY: SSE2 is baseline on x86_64; reads are bounded by `i + 16 <= len`.
        unsafe { find_escape_sse2(hay) }
    }
    #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
    {
        memchr4(hay, b'&', b'<', b'>', b'"')
    }
}

#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
#[inline]
fn is_escape(b: u8) -> bool {
    matches!(b, b'&' | b'<' | b'>' | b'"')
}

#[cfg(target_arch = "aarch64")]
#[inline]
unsafe fn find_escape_neon(hay: &[u8]) -> Option<usize> {
    use core::arch::aarch64::*;
    let (amp, lt, gt, qt) = unsafe {
        (
            vdupq_n_u8(b'&'),
            vdupq_n_u8(b'<'),
            vdupq_n_u8(b'>'),
            vdupq_n_u8(b'"'),
        )
    };
    let mut i = 0;
    while i + 16 <= hay.len() {
        let m = unsafe {
            let v = vld1q_u8(hay.as_ptr().add(i));
            vorrq_u8(
                vorrq_u8(vceqq_u8(v, amp), vceqq_u8(v, lt)),
                vorrq_u8(vceqq_u8(v, gt), vceqq_u8(v, qt)),
            )
        };
        // Narrow each 16-bit lane to 4 bits → a 64-bit "nibble per byte" mask.
        let mask = unsafe {
            vget_lane_u64(
                vreinterpret_u64_u8(vshrn_n_u16(vreinterpretq_u16_u8(m), 4)),
                0,
            )
        };
        if mask != 0 {
            return Some(i + (mask.trailing_zeros() as usize >> 2));
        }
        i += 16;
    }
    while i < hay.len() {
        if is_escape(hay[i]) {
            return Some(i);
        }
        i += 1;
    }
    None
}

#[cfg(target_arch = "x86_64")]
#[inline]
unsafe fn find_escape_sse2(hay: &[u8]) -> Option<usize> {
    use core::arch::x86_64::*;
    let (amp, lt, gt, qt) = unsafe {
        (
            _mm_set1_epi8(b'&' as i8),
            _mm_set1_epi8(b'<' as i8),
            _mm_set1_epi8(b'>' as i8),
            _mm_set1_epi8(b'"' as i8),
        )
    };
    let mut i = 0;
    while i + 16 <= hay.len() {
        let mask = unsafe {
            let v = _mm_loadu_si128(hay.as_ptr().add(i) as *const __m128i);
            let m = _mm_or_si128(
                _mm_or_si128(_mm_cmpeq_epi8(v, amp), _mm_cmpeq_epi8(v, lt)),
                _mm_or_si128(_mm_cmpeq_epi8(v, gt), _mm_cmpeq_epi8(v, qt)),
            );
            _mm_movemask_epi8(m)
        };
        if mask != 0 {
            return Some(i + mask.trailing_zeros() as usize);
        }
        i += 16;
    }
    while i < hay.len() {
        if is_escape(hay[i]) {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Index of the first byte in `hay` equal to `a`, `b`, `c`, or `d` — a
/// dependency-free `memchr4`. The SIMD `find_escape` fallback on non-x86/non-arm.
#[inline]
#[cfg_attr(any(target_arch = "aarch64", target_arch = "x86_64"), allow(dead_code))]
pub(crate) fn memchr4(hay: &[u8], a: u8, b: u8, c: u8, d: u8) -> Option<usize> {
    let (ba, bb, bc, bd) = (broadcast(a), broadcast(b), broadcast(c), broadcast(d));
    let mut i = 0;
    while i + W <= hay.len() {
        let w = usize::from_le_bytes(hay[i..i + W].try_into().unwrap());
        let m = zero_lanes(w, ba) | zero_lanes(w, bb) | zero_lanes(w, bc) | zero_lanes(w, bd);
        if m != 0 {
            return Some(i + first_hit(m));
        }
        i += W;
    }
    while i < hay.len() {
        let x = hay[i];
        if x == a || x == b || x == c || x == d {
            return Some(i);
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scalar(hay: &[u8], a: u8, b: u8, c: u8) -> Option<usize> {
        hay.iter().position(|&x| x == a || x == b || x == c)
    }

    #[test]
    fn memchr1_matches_scalar_reference() {
        let bytes: Vec<u8> = (0u8..=255).cycle().take(300).collect();
        for len in 0..bytes.len() {
            let hay = &bytes[..len];
            for needle in [b'\n', b'x', 0u8, 255u8] {
                assert_eq!(
                    memchr1(hay, needle),
                    hay.iter().position(|&x| x == needle),
                    "len={len} needle={needle}"
                );
            }
        }
    }

    #[test]
    fn matches_scalar_reference() {
        // A spread of lengths (sub-word, word-crossing, multi-word) with
        // targets at every offset, plus none-present and all-present.
        let bytes: Vec<u8> = (0u8..=255).cycle().take(300).collect();
        for len in 0..bytes.len() {
            let hay = &bytes[..len];
            for (a, b, c) in [(b'&', b'<', b'>'), (b'\n', b'\n', b'\n'), (0, 1, 2)] {
                assert_eq!(
                    memchr3(hay, a, b, c),
                    scalar(hay, a, b, c),
                    "len={len} set=({a},{b},{c})"
                );
            }
        }
    }

    #[test]
    fn target_at_each_position_within_a_word() {
        for pos in 0..(3 * W) {
            let mut hay = vec![b'x'; 3 * W];
            hay[pos] = b'<';
            assert_eq!(memchr3(&hay, b'&', b'<', b'>'), Some(pos), "pos={pos}");
        }
    }

    #[test]
    fn empty_and_no_match() {
        assert_eq!(memchr3(b"", b'&', b'<', b'>'), None);
        assert_eq!(
            memchr3(b"plain prose, no specials here", b'&', b'<', b'>'),
            None
        );
    }

    #[test]
    fn find_escape_matches_scalar() {
        let escape_scalar = |h: &[u8]| {
            h.iter()
                .position(|&b| matches!(b, b'&' | b'<' | b'>' | b'"'))
        };
        // Every length over a byte spread (covers SIMD body + scalar tail).
        let bytes: Vec<u8> = (0u8..=255).cycle().take(400).collect();
        for len in 0..bytes.len() {
            let hay = &bytes[..len];
            assert_eq!(find_escape(hay), escape_scalar(hay), "len={len}");
        }
        // A special at every position within and across SIMD blocks.
        for pos in 0..40 {
            for &sp in &[b'&', b'<', b'>', b'"'] {
                let mut h = vec![b'x'; 40];
                h[pos] = sp;
                assert_eq!(find_escape(&h), Some(pos), "pos={pos} sp={sp}");
            }
        }
        assert_eq!(find_escape(b""), None);
        assert_eq!(find_escape(b"no specials at all here, just prose"), None);
    }
}
