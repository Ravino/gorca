// SPDX-License-Identifier: GPL-3.0-or-later
//! `common-prefix-chars`: length of the common prefix of two `char` slices.
//!
//! Serves: leaf-text diff for typing echo (`gorca-core` text/caret.rs finds where one run was
//! inserted or erased by scanning the common prefix of the old and new text).

use crate::Impl;
use crate::kernel_spec;

/// Elements compared per step of the shared body.
const LANES: usize = 8;

/// Specification: the first index where the slices differ, or the shorter length.
#[must_use]
pub fn common_prefix_portable(a: &[char], b: &[char]) -> usize {
    prefix_body(a, b)
}

/// Compare `LANES` elements at a time with no data-dependent branch inside the step (OR of the
/// XORs), so a wide target compiles the step to load, xor, test; then find the exact index.
// Required by ADR-0027 2.2: the body must be compiled inside each feature-enabled caller.
#[allow(clippy::inline_always)]
#[inline(always)]
pub(crate) fn prefix_body(a: &[char], b: &[char]) -> usize {
    let n = a.len().min(b.len());
    let (a, b) = (&a[..n], &b[..n]);
    let (av, _) = a.as_chunks::<LANES>();
    let (bv, _) = b.as_chunks::<LANES>();
    let mut i = 0;
    for (x, y) in av.iter().zip(bv) {
        let mut diff = 0u32;
        for (p, q) in x.iter().zip(y) {
            diff |= u32::from(*p) ^ u32::from(*q);
        }
        if diff != 0 {
            break;
        }
        i += LANES;
    }
    let tail_a = &a[i..];
    let tail_b = &b[i..];
    i + tail_a
        .iter()
        .zip(tail_b)
        .position(|(p, q)| p != q)
        .unwrap_or(tail_a.len())
}

type PrefixFn = fn(&[char], &[char]) -> usize;

kernel_spec!(pub static SPEC: PrefixFn = {
    id: "common-prefix-chars",
    portable: common_prefix_portable,
    min_len: 64,
    impls: IMPLS,
});

#[cfg(target_arch = "x86_64")]
const IMPLS: &[Impl<PrefixFn>] = &[crate::arch::common_prefix_x86::avx2::IMPL];
#[cfg(not(target_arch = "x86_64"))]
const IMPLS: &[Impl<PrefixFn>] = &[];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::defs::testutil::runnable;
    use proptest::prelude::*;

    fn oracle(a: &[char], b: &[char]) -> usize {
        let mut i = 0;
        while i < a.len() && i < b.len() && a[i] == b[i] {
            i += 1;
        }
        i
    }

    fn check(a: &[char], b: &[char]) {
        let want = oracle(a, b);
        assert_eq!(prefix_body(a, b), want);
        for (name, f) in runnable(&SPEC) {
            assert_eq!(f(a, b), want, "{name} lens {} {}", a.len(), b.len());
            assert_eq!(f(b, a), want, "{name} swapped");
        }
    }

    #[test]
    fn one_difference_at_every_position() {
        for len in 0..=130usize {
            let a: Vec<char> = (0..len)
                .map(|i| char::from(b'a' + u8::try_from(i % 26).unwrap_or(0)))
                .collect();
            check(&a, &a);
            for at in 0..len {
                let mut b = a.clone();
                b[at] = '\u{1F600}';
                check(&a, &b);
                // Also a different length after the difference.
                b.truncate(len.saturating_sub(1).max(at));
                check(&a, &b);
            }
            check(&a, &a[..len / 2]);
            check(&a[len / 2..], &a);
        }
    }

    #[test]
    fn all_alignments_and_huge_sparse_difference() {
        let base: Vec<char> = "эхо typing 😀 ".chars().cycle().take(2 << 20).collect();
        let mut other = base.clone();
        other[(1 << 20) + 17] = 'X';
        for off in 0..64 {
            check(&base[off..], &other[off..]);
        }
        check(&base, &base);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(50_000))]
        #[test]
        fn random_pairs(
            common in proptest::collection::vec(any::<char>(), 0..200),
            x in proptest::collection::vec(any::<char>(), 0..40),
            y in proptest::collection::vec(any::<char>(), 0..40),
        ) {
            let mut a = common.clone();
            a.extend(&x);
            let mut b = common;
            b.extend(&y);
            check(&a, &b);
        }
    }
}
