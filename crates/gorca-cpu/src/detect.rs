// SPDX-License-Identifier: GPL-3.0-or-later
//! One-time hardware detection.
//!
//! The authority is the standard library's detection macros (CPUID plus the XGETBV check that the
//! OS saves the wide register state), called from `arch/` only. Raw CPUID words are kept for
//! diagnostics and for the hybrid-core symmetry check.
//!
//! **Hybrid CPUs (Intel P/E/LP-E cores, Arm big.LITTLE):** a kernel table is process-wide and a
//! thread can migrate between cores at any instruction, so every core of a package must report
//! the same instruction set. Intel guarantees this for x86 hybrid parts (AVX-512 is fused off on
//! the whole package when the E cores lack it) and Linux arm64 exposes the common subset of the
//! cores in `HWCAP`. `tests/symmetry.rs` pins a thread to each online CPU and compares
//! [`raw_isa`] across them; a machine that fails it must run with `GORCA_CPU_DISABLE`.

use crate::feature::FeatureSet;

/// Identification of the CPU model, for the diagnostic report. Empty off x86.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CpuInfo {
    pub vendor: String,
    pub brand: String,
    pub family: u32,
    pub model: u32,
    pub stepping: u32,
}

/// The ISA-describing CPUID words of one core (x86 only; empty elsewhere), per-core identifiers
/// excluded. Equal on all cores of a symmetric package.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RawIsa {
    pub words: Vec<u32>,
    /// XCR0: the register state the OS enabled.
    pub xcr0: u64,
}

/// Features the hardware and the OS support on this process, ignoring any disable list.
/// Cheap after the first call (std caches the result).
#[must_use]
pub fn detect() -> FeatureSet {
    #[cfg(target_arch = "x86_64")]
    {
        crate::arch::x86::detect()
    }
    #[cfg(target_arch = "aarch64")]
    {
        crate::arch::aarch64::detect()
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        FeatureSet::EMPTY
    }
}

/// Vendor and model of the core this thread runs on.
#[must_use]
pub fn cpu_info() -> CpuInfo {
    #[cfg(target_arch = "x86_64")]
    {
        crate::arch::x86::info()
    }
    #[cfg(target_arch = "aarch64")]
    {
        crate::arch::aarch64::info()
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        CpuInfo::default()
    }
}

/// Raw ISA words of the core this thread runs on right now (not cached).
#[must_use]
pub fn raw_isa() -> RawIsa {
    #[cfg(target_arch = "x86_64")]
    {
        crate::arch::x86::raw_isa()
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        RawIsa::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feature::Feature;

    #[test]
    fn detection_is_stable_and_consistent() {
        let a = detect();
        assert_eq!(a, detect());
        // Implication chains the hardware must satisfy.
        if a.contains(Feature::Avx2) {
            assert!(a.contains(Feature::Avx));
        }
        if a.contains(Feature::Avx512Bw) {
            assert!(a.contains(Feature::Avx512F));
        }
        if a.contains(Feature::Sse42) {
            assert!(a.contains(Feature::Sse41));
        }
        #[cfg(target_arch = "aarch64")]
        assert!(a.contains(Feature::Neon));
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn raw_isa_matches_std_for_avx2() {
        let r = raw_isa();
        // CPUID.(7,0):EBX bit 5 is AVX2; std additionally requires OS support.
        let hw = r.words[2] & (1 << 5) != 0;
        let os = r.xcr0 & 0b110 == 0b110;
        assert_eq!(detect().contains(Feature::Avx2), hw && os);
    }
}
