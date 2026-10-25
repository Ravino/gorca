// SPDX-License-Identifier: GPL-3.0-or-later
//! `utf8-scalar-count`: the number of Unicode scalars in a `&str` (`str::chars().count()`).
//!
//! Serves: text fetch and offset conversion (`text.chars().count()` runs on every text window,
//! caret and splice reply). Spec: the standard library's `chars().count()`.

use crate::Impl;
use crate::kernel_spec;

/// Specification: the standard library (already word-at-a-time on the baseline).
#[must_use]
pub fn utf8_scalar_count_portable(s: &str) -> usize {
    s.chars().count()
}

type CountFn = fn(&str) -> usize;

kernel_spec!(pub static SPEC: CountFn = {
    id: "utf8-scalar-count",
    portable: utf8_scalar_count_portable,
    min_len: 128,
    impls: IMPLS,
});

#[cfg(target_arch = "x86_64")]
const IMPLS: &[Impl<CountFn>] = &[crate::arch::utf8_scalar_count_x86::avx2::IMPL];
#[cfg(not(target_arch = "x86_64"))]
const IMPLS: &[Impl<CountFn>] = &[];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::defs::testutil::runnable;
    use proptest::prelude::*;

    fn oracle(s: &str) -> usize {
        let mut n = 0;
        for _ in s.chars() {
            n += 1;
        }
        n
    }

    fn check(s: &str) {
        let want = oracle(s);
        for (name, f) in runnable(&SPEC) {
            assert_eq!(f(s), want, "{name} on {} bytes", s.len());
        }
        #[cfg(target_arch = "x86_64")]
        if let Some(n) = crate::arch::utf8_scalar_count_x86::avx2::call_checked(s) {
            assert_eq!(n, want);
        }
    }

    #[test]
    fn every_length_and_offset_around_vector_widths() {
        let pool = "aб😀éz ";
        let base: String = pool.chars().cycle().take(700).collect();
        // Start at every char boundary of the first 70 bytes, take every length up to 140 bytes.
        for (start, _) in base.char_indices().take(70) {
            for (end, _) in base[start..].char_indices().take(140) {
                check(&base[start..start + end]);
            }
        }
        let ascii = "x".repeat(4 * 252 * 32 * 2 + 31);
        for n in [
            0,
            1,
            31,
            32,
            33,
            4 * 252 * 32 - 1,
            4 * 252 * 32,
            4 * 252 * 32 + 1,
            ascii.len(),
        ] {
            check(&ascii[..n]);
        }
        check(&"😀".repeat(3000));
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(20_000))]
        #[test]
        fn random_strings(s in "\\PC{0,200}") {
            check(&s);
        }

        #[test]
        fn mostly_ascii(s in "[ -~]{0,300}[а-я😀]{0,3}[ -~]{0,300}") {
            check(&s);
        }
    }
}
