// SPDX-License-Identifier: GPL-3.0-or-later
//! aarch64 detection. NEON (ASIMD) is part of the architecture baseline, so it is always
//! reported and never needs a log line; the optional extensions go through the std macro.

use crate::detect::CpuInfo;
use crate::feature::{Feature, FeatureSet};

pub(crate) fn detect() -> FeatureSet {
    let mut s = FeatureSet::EMPTY;
    s.insert(Feature::Neon);
    macro_rules! d {
        ($($f:ident => $n:tt),+ $(,)?) => {
            $( if std::arch::is_aarch64_feature_detected!($n) { s.insert(Feature::$f); } )+
        };
    }
    d! {
        Dotprod => "dotprod", I8mm => "i8mm", Bf16 => "bf16", Fp16 => "fp16",
        Pmull => "pmull", Sha2 => "sha2", Crc => "crc", Lse => "lse",
        Sve => "sve", Sve2 => "sve2", Aes => "aes",
    }
    s
}

pub(crate) fn info() -> CpuInfo {
    CpuInfo::default()
}
