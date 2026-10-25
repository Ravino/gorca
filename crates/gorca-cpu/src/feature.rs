// SPDX-License-Identifier: GPL-3.0-or-later
//! The closed set of CPU features gorca can dispatch on, and a bit set of them.
//!
//! Baseline features are not listed: SSE2 on x86-64 and NEON on aarch64 are always present in a
//! gorca build (ADR-0027 section 3.1), so they need neither detection nor a log line.

use std::fmt;

macro_rules! features {
    ($( $(#[$m:meta])* $var:ident => ($name:literal, $label:literal, $arch:ident) ),+ $(,)?) => {
        /// One CPU feature. `name` is the spelling of `is_x86_feature_detected!` and
        /// `#[target_feature(enable = ...)]`.
        #[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
        #[non_exhaustive]
        pub enum Feature {
            $( $(#[$m])* $var ),+
        }

        impl Feature {
            /// Every feature, in the order the log and the reports use.
            pub const ALL: &'static [Feature] = &[ $( Feature::$var ),+ ];

            /// The `target_feature` spelling (`"avx2"`, `"sse4.2"`).
            #[must_use]
            pub const fn name(self) -> &'static str {
                match self { $( Feature::$var => $name ),+ }
            }

            /// The spelling used in log lines (`"AVX2"`, `"SSE4.2"`).
            #[must_use]
            pub const fn label(self) -> &'static str {
                match self { $( Feature::$var => $label ),+ }
            }

            /// Which architecture the feature belongs to.
            #[must_use]
            pub const fn arch(self) -> Arch {
                match self { $( Feature::$var => Arch::$arch ),+ }
            }
        }

        /// One zero-sized type per feature, the type parameter of [`crate::Cap`].
        pub mod marker {
            use super::{Feature, FeatureMarker};
            $(
                #[derive(Copy, Clone, Debug)]
                pub struct $var;
                impl FeatureMarker for $var {
                    const FEATURE: Feature = Feature::$var;
                }
            )+
        }
    };
}

/// Type-level name of a [`Feature`] (see [`marker`]).
pub trait FeatureMarker: Copy {
    const FEATURE: Feature;
}

/// Architecture families gorca knows features for.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Arch {
    X86,
    Aarch64,
    /// Present on both (for example AES).
    Any,
}

features! {
    Sse3 => ("sse3", "SSE3", X86),
    Ssse3 => ("ssse3", "SSSE3", X86),
    Sse41 => ("sse4.1", "SSE4.1", X86),
    Sse42 => ("sse4.2", "SSE4.2", X86),
    Popcnt => ("popcnt", "POPCNT", X86),
    Lzcnt => ("lzcnt", "LZCNT", X86),
    Bmi1 => ("bmi1", "BMI1", X86),
    Bmi2 => ("bmi2", "BMI2", X86),
    Avx => ("avx", "AVX", X86),
    Avx2 => ("avx2", "AVX2", X86),
    Fma => ("fma", "FMA", X86),
    F16c => ("f16c", "F16C", X86),
    Pclmulqdq => ("pclmulqdq", "PCLMULQDQ", X86),
    Sha => ("sha", "SHA", X86),
    Gfni => ("gfni", "GFNI", X86),
    /// AVX-VNNI (VEX-encoded VNNI, Alder Lake and later; no AVX-512 needed).
    AvxVnni => ("avxvnni", "AVX-VNNI", X86),
    Avx512F => ("avx512f", "AVX-512F", X86),
    Avx512Cd => ("avx512cd", "AVX-512CD", X86),
    Avx512Bw => ("avx512bw", "AVX-512BW", X86),
    Avx512Dq => ("avx512dq", "AVX-512DQ", X86),
    Avx512Vl => ("avx512vl", "AVX-512VL", X86),
    Avx512Vbmi => ("avx512vbmi", "AVX-512VBMI", X86),
    Avx512Vnni => ("avx512vnni", "AVX-512VNNI", X86),
    Avx512Vpopcntdq => ("avx512vpopcntdq", "AVX-512VPOPCNTDQ", X86),
    Avx512Bf16 => ("avx512bf16", "AVX-512BF16", X86),
    /// AVX10.1 (converged vector ISA). Detected from CPUID leaves 7.1 and 0x24 plus the OS state.
    Avx10_1 => ("avx10.1", "AVX10.1", X86),
    Neon => ("neon", "NEON", Aarch64),
    Dotprod => ("dotprod", "DotProd", Aarch64),
    I8mm => ("i8mm", "I8MM", Aarch64),
    Bf16 => ("bf16", "BF16", Aarch64),
    Fp16 => ("fp16", "FP16", Aarch64),
    Pmull => ("pmull", "PMULL", Aarch64),
    Sha2 => ("sha2", "SHA2", Aarch64),
    Crc => ("crc", "CRC", Aarch64),
    Lse => ("lse", "LSE", Aarch64),
    Sve => ("sve", "SVE", Aarch64),
    Sve2 => ("sve2", "SVE2", Aarch64),
    Aes => ("aes", "AES", Any),
}

impl Feature {
    /// Parse a name from the disable list: `target_feature` spelling or label, case-insensitive,
    /// with `.` and `_` interchangeable (`sse4.2`, `SSE4_2`, `sse42`).
    #[must_use]
    pub fn parse(s: &str) -> Option<Feature> {
        fn norm(s: &str) -> String {
            s.chars()
                .filter(|c| !matches!(c, '.' | '_' | '-'))
                .flat_map(char::to_lowercase)
                .collect()
        }
        let want = norm(s);
        Feature::ALL
            .iter()
            .copied()
            .find(|f| norm(f.name()) == want || norm(f.label()) == want)
    }

    const fn bit(self) -> u64 {
        1u64 << (self as u8)
    }
}

impl fmt::Display for Feature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// A set of [`Feature`]s. `const`-constructible so that a kernel's `requires` is a constant.
#[derive(Copy, Clone, Default, Eq, PartialEq, Hash)]
pub struct FeatureSet(u64);

impl FeatureSet {
    pub const EMPTY: FeatureSet = FeatureSet(0);

    /// The set holding exactly `features`.
    #[must_use]
    pub const fn of(features: &[Feature]) -> FeatureSet {
        let mut bits = 0u64;
        let mut i = 0;
        while i < features.len() {
            bits |= features[i].bit();
            i += 1;
        }
        FeatureSet(bits)
    }

    #[must_use]
    pub const fn contains(self, f: Feature) -> bool {
        self.0 & f.bit() != 0
    }

    /// `self` is a subset of `other`.
    #[must_use]
    pub const fn is_subset_of(self, other: FeatureSet) -> bool {
        self.0 & !other.0 == 0
    }

    #[must_use]
    pub const fn union(self, other: FeatureSet) -> FeatureSet {
        FeatureSet(self.0 | other.0)
    }

    #[must_use]
    pub const fn difference(self, other: FeatureSet) -> FeatureSet {
        FeatureSet(self.0 & !other.0)
    }

    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub fn insert(&mut self, f: Feature) {
        self.0 |= f.bit();
    }

    pub fn remove(&mut self, f: Feature) {
        self.0 &= !f.bit();
    }

    /// Members in [`Feature::ALL`] order.
    pub fn iter(self) -> impl Iterator<Item = Feature> {
        Feature::ALL
            .iter()
            .copied()
            .filter(move |f| self.contains(*f))
    }

    /// Raw bits, stable only within one build.
    #[must_use]
    pub const fn bits(self) -> u64 {
        self.0
    }
}

impl fmt::Debug for FeatureSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set().entries(self.iter()).finish()
    }
}

impl FromIterator<Feature> for FeatureSet {
    fn from_iter<T: IntoIterator<Item = Feature>>(iter: T) -> Self {
        let mut s = FeatureSet::EMPTY;
        for f in iter {
            s.insert(f);
        }
        s
    }
}

/// Compile-time equality of two `target_feature` spellings (used by `kernel_impl!`).
#[doc(hidden)]
#[must_use]
pub const fn same_name(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn at_most_64_features_and_names_are_unique() {
        assert!(Feature::ALL.len() <= 64);
        for (i, a) in Feature::ALL.iter().enumerate() {
            for b in &Feature::ALL[i + 1..] {
                assert_ne!(a.name(), b.name());
                assert_ne!(a.label(), b.label());
            }
        }
    }

    #[test]
    fn parse_accepts_spellings() {
        assert_eq!(Feature::parse("avx2"), Some(Feature::Avx2));
        assert_eq!(Feature::parse("AVX2"), Some(Feature::Avx2));
        assert_eq!(Feature::parse("sse4.2"), Some(Feature::Sse42));
        assert_eq!(Feature::parse("SSE42"), Some(Feature::Sse42));
        assert_eq!(Feature::parse("sse4_2"), Some(Feature::Sse42));
        assert_eq!(Feature::parse("avx-vnni"), Some(Feature::AvxVnni));
        assert_eq!(Feature::parse("avx10.1"), Some(Feature::Avx10_1));
        assert_eq!(Feature::parse("nope"), None);
    }

    #[test]
    fn set_algebra() {
        let a = FeatureSet::of(&[Feature::Avx2, Feature::Bmi2]);
        let b = FeatureSet::of(&[Feature::Avx2]);
        assert!(b.is_subset_of(a));
        assert!(!a.is_subset_of(b));
        assert_eq!(a.difference(b), FeatureSet::of(&[Feature::Bmi2]));
        assert_eq!(a.iter().collect::<Vec<_>>(), [Feature::Bmi2, Feature::Avx2]);
        assert!(FeatureSet::EMPTY.is_subset_of(b));
    }

    #[test]
    fn same_name_is_exact() {
        assert!(same_name("avx2", "avx2"));
        assert!(!same_name("avx2", "avx"));
        assert!(!same_name("avx2", "bmi2"));
    }
}
