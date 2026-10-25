// SPDX-License-Identifier: GPL-3.0-or-later
//! `common-prefix-chars` on x86-64, AVX2 (tier 3; tier 2, the autovectorised portable body,
//! measured only 1.3x at 4 KiB).
//!
//! 32 `char`s (four `ymm` pairs) per step: `vpcmpeqd` the pairs, AND the four masks, one
//! `vpmovmskb` decides whether the step is equal. The first unequal step is re-scanned eight
//! elements at a time with `vmovmskps`, which gives the exact index through `trailing_zeros`.
#![allow(unsafe_code)]
// Every load and store here is the unaligned form (`loadu`/`storeu`): no alignment precondition.
#![allow(clippy::cast_ptr_alignment)]

use core::arch::x86_64::{
    __m256i, _mm256_and_si256, _mm256_castsi256_ps, _mm256_cmpeq_epi32, _mm256_loadu_si256,
    _mm256_movemask_epi8, _mm256_movemask_ps,
};

use crate::kernel_impl;

kernel_impl! {
    /// See the module docs.
    pub mod avx2 ["avx2" => Avx2] (a: &[char], b: &[char]) -> usize {
        let n = a.len().min(b.len());
        let (pa, pb) = (a.as_ptr(), b.as_ptr());
        let mut i = 0usize;
        // 32 elements per step. `i + 32 <= n <= a.len(), b.len()`, so every load below reads
        // inside both slices (`char` is 4 bytes, so a 32-byte load covers 8 elements).
        while i + 32 <= n {
            // SAFETY: elements `i..i + 32` of `a` and `b` exist (loop condition); the four
            // 8-element loads cover exactly them; `loadu` needs no alignment.
            let all = unsafe {
                let eq = |k: usize| {
                    _mm256_cmpeq_epi32(
                        _mm256_loadu_si256(pa.add(i + k).cast::<__m256i>()),
                        _mm256_loadu_si256(pb.add(i + k).cast::<__m256i>()),
                    )
                };
                _mm256_and_si256(_mm256_and_si256(eq(0), eq(8)), _mm256_and_si256(eq(16), eq(24)))
            };
            if _mm256_movemask_epi8(all) != -1 {
                break;
            }
            i += 32;
        }
        while i + 8 <= n {
            // SAFETY: elements `i..i + 8` of both slices exist (loop condition).
            let eq = unsafe {
                _mm256_cmpeq_epi32(
                    _mm256_loadu_si256(pa.add(i).cast::<__m256i>()),
                    _mm256_loadu_si256(pb.add(i).cast::<__m256i>()),
                )
            };
            let mask = _mm256_movemask_ps(_mm256_castsi256_ps(eq)).cast_unsigned();
            if mask != 0xFF {
                return i + (!mask).trailing_zeros() as usize;
            }
            i += 8;
        }
        while i < n && a.get(i) == b.get(i) {
            i += 1;
        }
        i
    }
}
