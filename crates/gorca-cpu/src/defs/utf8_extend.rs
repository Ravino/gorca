// SPDX-License-Identifier: GPL-3.0-or-later
//! `utf8-extend-chars`: append the scalars of a `&str` to a `Vec<char>`.
//!
//! Serves: filling and splicing the text window (`chars.extend(text.chars())`,
//! `text.chars().collect::<Vec<_>>()` in the native text layer).

use crate::Impl;
use crate::kernel_spec;

/// Specification: the standard library iterator.
pub fn utf8_extend_portable(s: &str, out: &mut Vec<char>) {
    out.extend(s.chars());
}

/// Bytes per ASCII probe.
const RUN: usize = 32;

/// Find the leading run of ASCII in probes of 32 bytes (a vectorised OR-reduce each), widen that
/// run to `char`s in one pass without decoding, and hand the rest (a partial probe or everything
/// from the first non-ASCII byte on) to the standard decoder. Text that starts with non-ASCII
/// costs one probe more than the portable function, no more.
// Required by ADR-0027 2.2: the body must be compiled inside each feature-enabled caller.
#[allow(clippy::inline_always)]
#[inline(always)]
pub(crate) fn extend_body(s: &str, out: &mut Vec<char>) {
    let bytes = s.as_bytes();
    let mut k = 0;
    while let Some(probe) = bytes.get(k..k + RUN) {
        if !probe.iter().all(u8::is_ascii) {
            break;
        }
        k += RUN;
    }
    if let Some(head) = bytes.get(..k) {
        out.extend(head.iter().map(|&b| char::from(b)));
    }
    // `k` follows only ASCII bytes, so it is a scalar boundary.
    if let Some(rest) = s.get(k..) {
        out.extend(rest.chars());
    }
}

type ExtendFn = fn(&str, &mut Vec<char>);

kernel_spec!(pub static SPEC: ExtendFn = {
    id: "utf8-extend-chars",
    portable: utf8_extend_portable,
    min_len: 64,
    impls: IMPLS,
});

#[cfg(target_arch = "x86_64")]
const IMPLS: &[Impl<ExtendFn>] = &[crate::arch::utf8_extend_x86::avx2::IMPL];
#[cfg(not(target_arch = "x86_64"))]
const IMPLS: &[Impl<ExtendFn>] = &[];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::defs::testutil::runnable;
    use proptest::prelude::*;

    fn check(s: &str) {
        let want: Vec<char> = s.chars().collect();
        for (name, f) in runnable(&SPEC) {
            let mut v = vec!['\u{1}'; 3];
            f(s, &mut v);
            assert_eq!(&v[..3], &['\u{1}'; 3], "{name} clobbered the prefix");
            assert_eq!(&v[3..], &want[..], "{name} on {} bytes", s.len());
        }
        let mut v = Vec::new();
        extend_body(s, &mut v);
        assert_eq!(v, want);
    }

    #[test]
    fn every_length_and_offset_around_the_probe_width() {
        let pool = "aб😀éz ";
        let base: String = pool.chars().cycle().take(400).collect();
        for (start, _) in base.char_indices().take(70) {
            for (end, _) in base[start..].char_indices().take(140) {
                check(&base[start..start + end]);
            }
        }
        for n in [0usize, 1, 31, 32, 33, 63, 64, 65, 1000] {
            check(&"x".repeat(n));
            check(&format!("{}я{}", "x".repeat(n), "y".repeat(n)));
        }
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
