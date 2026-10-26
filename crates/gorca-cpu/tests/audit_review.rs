// SPDX-License-Identifier: GPL-3.0-or-later
//! Reviewer audit (ADR-0027 implementation review r1).
//!
//! - `exhaustive_*`: every kernel the hardware can run against the portable spec at every length
//!   0..=4100 and several unaligned starts. Run in release: `cargo test --release -p gorca-cpu
//!   --test audit_review`.
//! - `audit_safe_resolve_*`: the safe public resolver accepts a caller-made `FeatureSet`, so safe
//!   code can obtain a `#[target_feature]` trampoline the process may not run. Fails under
//!   `GORCA_CPU_DISABLE=avx2` on an AVX2 host; on a CPU without AVX2 the same call is UB.

use gorca_cpu::specs::{COMMON_PREFIX, UTF8_EXTEND, UTF8_SCALAR_COUNT};
use gorca_cpu::{DisableList, Enabled, Feature, FeatureSet, Kernels, detect};

const OFFSETS: [usize; 6] = [0, 1, 3, 7, 13, 31];

fn contents() -> Vec<(&'static str, String)> {
    let n = 4100 + 64;
    vec![
        ("ascii", "x".repeat(n)),
        ("4-byte", "😀".repeat(n / 4 + 1)),
        ("mixed", "aб😀éz ".chars().cycle().take(n).collect()),
        ("cjk", "中文字".chars().cycle().take(n).collect()),
        // A non-ASCII char exactly every 31 bytes: continuation bytes straddle every lane edge.
        (
            "periodic",
            "abcdefghijklmnopqrstuvwxyzabcdяё"
                .chars()
                .cycle()
                .take(n)
                .collect(),
        ),
    ]
}

/// The largest char boundary `<= at`.
fn floor(s: &str, mut at: usize) -> usize {
    at = at.min(s.len());
    while !s.is_char_boundary(at) {
        at -= 1;
    }
    at
}

#[test]
fn exhaustive_scalar_count_and_extend() {
    let (count, _) = UTF8_SCALAR_COUNT.resolve(Enabled::detected());
    let (extend, _) = UTF8_EXTEND.resolve(Enabled::detected());
    for (name, base) in contents() {
        for off in OFFSETS {
            let start = floor(&base, off);
            let tail = &base[start..];
            for len in 0..=4100usize {
                let end = floor(tail, len);
                let s = &tail[..end];
                let want = s.chars().count();
                assert_eq!(count(s), want, "count {name} off {off} len {len}");
                assert_eq!(
                    gorca_cpu::text::utf8_scalar_count(s),
                    want,
                    "wrapper {name} off {off} len {len}"
                );
                if len % 7 == 0 || len < 300 {
                    let mut v = vec!['!'];
                    extend(s, &mut v);
                    assert_eq!(v.len(), want + 1, "extend {name} off {off} len {len}");
                    assert!(
                        v[1..].iter().copied().eq(s.chars()),
                        "extend {name} {off} {len}"
                    );
                }
            }
        }
    }
}

#[test]
fn exhaustive_common_prefix() {
    let (prefix, _) = COMMON_PREFIX.resolve(Enabled::detected());
    let a: Vec<char> = "aб😀éz ".chars().cycle().take(4200).collect();
    for off in [0usize, 1, 3, 7] {
        let a = &a[off..];
        for len in (0..=4100usize).filter(|l| *l < 300 || l % 13 == 0 || l % 32 <= 1) {
            let x = &a[..len];
            assert_eq!(prefix(x, x), len, "equal len {len} off {off}");
            for at in [0, len / 2, len.saturating_sub(1), len.saturating_sub(33)] {
                if at >= len {
                    continue;
                }
                let mut y = x.to_vec();
                y[at] = '\u{10FFFF}';
                assert_eq!(prefix(x, &y), at, "diff at {at} len {len} off {off}");
                assert_eq!(prefix(&y[..=at], x), at, "short b at {at}");
            }
        }
    }
}

#[test]
fn safe_resolve_cannot_select_what_the_process_may_not_run() {
    // `Enabled` cannot be built from a caller-made set: the only values are detection, the
    // disable filter, and narrowing of those.
    let en = DisableList::parse("avx2").enabled();
    assert!(!en.contains(Feature::Avx2));
    let (_, c) = UTF8_SCALAR_COUNT.resolve(en);
    assert!(c.is_portable(), "{c:?}");
    let (_, cs) = Kernels::resolve(en);
    for c in cs {
        assert!(c.requires.is_subset_of(en.set()), "Kernels::resolve: {c:?}");
    }
    assert!(UTF8_SCALAR_COUNT.impls[0].checked(en).is_none());
    // Narrowing a detected set by a mask never adds features.
    let narrowed = Enabled::detected().restrict(FeatureSet::of(&[Feature::Avx2]));
    assert!(narrowed.set().is_subset_of(detect()));
    assert!(Enabled::none().set().is_empty());
}

#[test]
fn disabling_avx_also_disables_avx2_kernels() {
    let enabled = DisableList::parse("avx").apply(detect());
    assert!(!enabled.contains(Feature::Avx2), "{enabled:?}");
    let (_, c) = UTF8_SCALAR_COUNT.resolve(DisableList::parse("avx").enabled());
    assert!(c.is_portable());
}
