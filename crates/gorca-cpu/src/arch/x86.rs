// SPDX-License-Identifier: GPL-3.0-or-later
//! x86-64 detection. `unsafe` here is the CPUID and XGETBV instructions only (reason for the
//! module-level allow, ADR-0027 CD-9).
#![allow(unsafe_code)]

use core::arch::x86_64::{__cpuid, __cpuid_count, _xgetbv};

use crate::detect::{CpuInfo, RawIsa};
use crate::feature::{Feature, FeatureSet};

/// Detect through `is_x86_feature_detected!`, which also checks that the OS enabled the register
/// state (XGETBV), and cache the answer in std. This is the one place that calls the macro
/// (ADR-0027 CD-1).
pub(crate) fn detect() -> FeatureSet {
    let mut s = FeatureSet::EMPTY;
    macro_rules! d {
        ($($f:ident => $n:tt),+ $(,)?) => {
            $( if std::arch::is_x86_feature_detected!($n) { s.insert(Feature::$f); } )+
        };
    }
    d! {
        Sse3 => "sse3", Ssse3 => "ssse3", Sse41 => "sse4.1", Sse42 => "sse4.2",
        Popcnt => "popcnt", Lzcnt => "lzcnt", Bmi1 => "bmi1", Bmi2 => "bmi2",
        Avx => "avx", Avx2 => "avx2", Fma => "fma", F16c => "f16c",
        Pclmulqdq => "pclmulqdq", Aes => "aes", Sha => "sha", Gfni => "gfni",
        AvxVnni => "avxvnni",
        Avx512F => "avx512f", Avx512Cd => "avx512cd", Avx512Bw => "avx512bw",
        Avx512Dq => "avx512dq", Avx512Vl => "avx512vl", Avx512Vbmi => "avx512vbmi",
        Avx512Vnni => "avx512vnni", Avx512Vpopcntdq => "avx512vpopcntdq",
        Avx512Bf16 => "avx512bf16",
    }
    if avx10_1(&raw_isa()) {
        s.insert(Feature::Avx10_1);
    }
    s
}

fn max_leaf() -> u32 {
    __cpuid(0).eax
}

/// The ISA-describing CPUID words of the core this thread runs on. Fields that differ per core
/// (APIC id in leaf 1 EBX) are left out, so two cores of one package must produce equal values.
pub(crate) fn raw_isa() -> RawIsa {
    let max = max_leaf();
    let l1 = __cpuid(1);
    let (l7, l71) = if max >= 7 {
        (__cpuid_count(7, 0), __cpuid_count(7, 1))
    } else {
        (__cpuid(0), __cpuid(0))
    };
    let ext = __cpuid(0x8000_0000).eax;
    let l81 = if ext >= 0x8000_0001 {
        __cpuid(0x8000_0001)
    } else {
        __cpuid(0)
    };
    let avx10 = if max >= 0x24 {
        __cpuid_count(0x24, 0).ebx
    } else {
        0
    };
    // XCR0 is only readable when the OS enabled XSAVE (CPUID.1:ECX bit 27 OSXSAVE).
    let xcr0 = if l1.ecx & (1 << 27) != 0 {
        // SAFETY: OSXSAVE is set, so XGETBV with ECX=0 is defined.
        unsafe { _xgetbv(0) }
    } else {
        0
    };
    RawIsa {
        words: vec![
            l1.ecx, l1.edx, l7.ebx, l7.ecx, l7.edx, l71.eax, l71.edx, l81.ecx, l81.edx, avx10,
        ],
        xcr0,
    }
}

/// AVX10.1: CPUID.(7,1):EDX bit 19, leaf 0x24 version >= 1, and the OS enabled the SSE, AVX and
/// AVX-512 (opmask, ZMM) state. AVX10 uses the opmask registers even at 256-bit width.
fn avx10_1(r: &RawIsa) -> bool {
    const XCR0_NEEDED: u64 = 0b1110_0110;
    let (edx71, ver) = (r.words[6], r.words[9] & 0xff);
    edx71 & (1 << 19) != 0 && ver >= 1 && r.xcr0 & XCR0_NEEDED == XCR0_NEEDED
}

/// Vendor, brand string and family/model/stepping of the current core.
pub(crate) fn info() -> CpuInfo {
    let l0 = __cpuid(0);
    let mut vendor = Vec::with_capacity(12);
    for r in [l0.ebx, l0.edx, l0.ecx] {
        vendor.extend_from_slice(&r.to_le_bytes());
    }
    let ext = __cpuid(0x8000_0000).eax;
    let mut brand = Vec::with_capacity(48);
    if ext >= 0x8000_0004 {
        for leaf in 0x8000_0002..=0x8000_0004 {
            let r = __cpuid(leaf);
            for w in [r.eax, r.ebx, r.ecx, r.edx] {
                brand.extend_from_slice(&w.to_le_bytes());
            }
        }
    }
    let sig = __cpuid(1).eax;
    let (mut family, mut model) = ((sig >> 8) & 0xf, (sig >> 4) & 0xf);
    if family == 0xf {
        family += (sig >> 20) & 0xff;
    }
    if family == 0x6 || family == 0xf {
        model += ((sig >> 16) & 0xf) << 4;
    }
    let text = |b: &[u8]| {
        String::from_utf8_lossy(b)
            .trim_matches(|c: char| c == '\0' || c.is_whitespace())
            .to_owned()
    };
    CpuInfo {
        vendor: text(&vendor),
        brand: text(&brand),
        family,
        model,
        stepping: sig & 0xf,
    }
}
