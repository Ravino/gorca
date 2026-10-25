// SPDX-License-Identifier: GPL-3.0-or-later
//! `utf8-scalar-count` on x86-64, AVX2 (tier 3: the autovectorised loop measured 6x *slower*
//! than std's word-at-a-time `chars().count()`, so this one is written with intrinsics).
//!
//! Counts the bytes that are not UTF-8 continuation bytes (`0b10xxxxxx`, signed -128..=-65),
//! which for valid UTF-8 is the scalar count. 128 bytes per step: four `vpcmpgtb` masks
//! subtracted from four per-lane `u8` counters, reduced with `vpsadbw` before any counter can
//! reach 256 (at most 255 increments: 252 quads plus up to 3 loose vectors).
#![allow(unsafe_code)]
// Every load and store here is the unaligned form (`loadu`/`storeu`): no alignment precondition.
#![allow(clippy::cast_ptr_alignment)]

use core::arch::x86_64::{
    __m256i, _mm256_add_epi64, _mm256_cmpgt_epi8, _mm256_loadu_si256, _mm256_sad_epu8,
    _mm256_set1_epi8, _mm256_setzero_si256, _mm256_storeu_si256, _mm256_sub_epi8,
};

use crate::kernel_impl;

kernel_impl! {
    /// See the module docs.
    pub mod avx2 ["avx2" => Avx2] (s: &str) -> usize {
        let (vecs, tail) = s.as_bytes().as_chunks::<32>();
        let thr = _mm256_set1_epi8(-65);
        let zero = _mm256_setzero_si256();
        let mut sums = zero;
        for block in vecs.chunks(4 * 252) {
            let (quads, rest) = block.as_chunks::<4>();
            let (mut c0, mut c1, mut c2, mut c3) = (zero, zero, zero, zero);
            for q in quads {
                let p = q.as_ptr().cast::<__m256i>();
                // SAFETY: `q` is `[[u8; 32]; 4]`: 128 readable bytes, so the four 32-byte
                // loads at `p`, `p + 1`, `p + 2`, `p + 3` stay inside it; `loadu` has no
                // alignment requirement.
                let (v0, v1, v2, v3) = unsafe {
                    (
                        _mm256_loadu_si256(p),
                        _mm256_loadu_si256(p.add(1)),
                        _mm256_loadu_si256(p.add(2)),
                        _mm256_loadu_si256(p.add(3)),
                    )
                };
                // A lane is -1 where the byte is not a continuation byte; subtracting adds 1.
                c0 = _mm256_sub_epi8(c0, _mm256_cmpgt_epi8(v0, thr));
                c1 = _mm256_sub_epi8(c1, _mm256_cmpgt_epi8(v1, thr));
                c2 = _mm256_sub_epi8(c2, _mm256_cmpgt_epi8(v2, thr));
                c3 = _mm256_sub_epi8(c3, _mm256_cmpgt_epi8(v3, thr));
            }
            for v in rest {
                // SAFETY: `v` is `[u8; 32]`, one full unaligned load.
                let x = unsafe { _mm256_loadu_si256(v.as_ptr().cast::<__m256i>()) };
                c0 = _mm256_sub_epi8(c0, _mm256_cmpgt_epi8(x, thr));
            }
            // Each counter took at most 255 increments (252 + 3): no lane overflowed.
            let lo = _mm256_add_epi64(_mm256_sad_epu8(c0, zero), _mm256_sad_epu8(c1, zero));
            let hi = _mm256_add_epi64(_mm256_sad_epu8(c2, zero), _mm256_sad_epu8(c3, zero));
            sums = _mm256_add_epi64(sums, _mm256_add_epi64(lo, hi));
        }
        let mut lanes = [0u64; 4];
        // SAFETY: `lanes` is 32 bytes; `storeu` has no alignment requirement.
        unsafe { _mm256_storeu_si256(lanes.as_mut_ptr().cast::<__m256i>(), sums) };
        let head: u64 = lanes.iter().sum();
        let tail = tail.iter().filter(|b| b.cast_signed() >= -0x40).count();
        usize::try_from(head).unwrap_or(usize::MAX).saturating_add(tail)
    }
}
