// SPDX-License-Identifier: GPL-3.0-or-later
//! Kernel specifications: the portable function (the specification), the faster implementations
//! in preference order, and the resolution rule (ADR-0027 section 2.3).

use crate::enabled::Enabled;
use crate::feature::FeatureSet;

/// One implementation of a kernel and the features it needs. Built by `kernel_impl!`, which
/// derives `requires` and the `#[target_feature]` attributes from one list.
#[derive(Debug)]
pub struct Impl<F: 'static> {
    pub name: &'static str,
    pub requires: FeatureSet,
    /// Private: calling it on a CPU without `requires` is undefined behaviour, so the only
    /// ways out are the resolver and [`Impl::checked`].
    f: F,
}

impl<F: Copy> Impl<F> {
    /// Build an entry. `requires` must list every feature `f` enables (`kernel_impl!` does).
    #[must_use]
    pub const fn new(name: &'static str, requires: FeatureSet, f: F) -> Self {
        Self { name, requires, f }
    }

    /// The function, if every required feature is in `have`.
    #[must_use]
    pub fn checked(&self, have: Enabled) -> Option<F> {
        self.requires.is_subset_of(have.set()).then_some(self.f)
    }
}

/// A kernel: the portable specification plus implementations, best first.
#[derive(Debug)]
pub struct KernelSpec<F: 'static> {
    /// Stable identifier used in logs, the doctor report and `hints.toml`.
    pub id: &'static str,
    /// Safe Rust, always present, the reference in every equivalence test.
    pub portable: F,
    pub impls: &'static [Impl<F>],
    /// Inputs shorter than this use the portable function inline (CD-2). Set from the benchmark.
    pub min_len: usize,
}

/// What resolution picked for one kernel.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Choice {
    pub kernel: &'static str,
    /// `"portable"` or the implementation's name.
    pub name: &'static str,
    pub requires: FeatureSet,
}

impl Choice {
    #[must_use]
    pub fn is_portable(&self) -> bool {
        self.name == PORTABLE
    }
}

/// The name of the portable implementation in reports.
pub const PORTABLE: &str = "portable";

impl<F: Copy> KernelSpec<F> {
    /// The first implementation whose `requires` are all in `enabled`, else the portable one.
    #[must_use]
    pub fn resolve(&self, enabled: Enabled) -> (F, Choice) {
        for i in self.impls {
            if i.requires.is_subset_of(enabled.set()) {
                return (
                    i.f,
                    Choice {
                        kernel: self.id,
                        name: i.name,
                        requires: i.requires,
                    },
                );
            }
        }
        (
            self.portable,
            Choice {
                kernel: self.id,
                name: PORTABLE,
                requires: FeatureSet::EMPTY,
            },
        )
    }

    /// Union of the features of all implementations (the k of the 2^k resolver test).
    #[must_use]
    pub fn features(&self) -> FeatureSet {
        self.impls
            .iter()
            .fold(FeatureSet::EMPTY, |a, i| a.union(i.requires))
    }

    /// Type-erased description for reports and tests.
    #[must_use]
    pub fn info(&self) -> KernelInfo {
        KernelInfo {
            id: self.id,
            min_len: self.min_len,
            impls: self.impls.iter().map(|i| (i.name, i.requires)).collect(),
        }
    }
}

/// A kernel without its function type.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KernelInfo {
    pub id: &'static str,
    pub min_len: usize,
    /// Preference order, best first.
    pub impls: Vec<(&'static str, FeatureSet)>,
}

/// Define a [`KernelSpec`]-typed `static`.
///
/// ```ignore
/// kernel_spec!(pub static SPEC: fn(&str) -> usize = {
///     id: "utf8-scalar-count", portable: portable, min_len: 64, impls: IMPLS,
/// });
/// ```
#[macro_export]
macro_rules! kernel_spec {
    ($vis:vis static $name:ident : $fty:ty = {
        id: $id:literal, portable: $portable:path, min_len: $min:expr, impls: $impls:expr $(,)?
    }) => {
        $vis static $name: $crate::KernelSpec<$fty> = $crate::KernelSpec {
            id: $id,
            portable: $portable,
            impls: $impls,
            min_len: $min,
        };
    };
}

/// Generate an implementation of a kernel from one list (ADR-0027 section 2.3, review H-1).
///
/// ```ignore
/// kernel_impl! {
///     /// docs
///     pub mod avx2_bmi2 ["avx2" => Avx2, "bmi2" => Bmi2] (a: &[u8], b: &[u8]) -> usize { shared_body(a, b) }
/// }
/// ```
///
/// expands to a module holding
/// - a private `#[target_feature(enable = ...)]` fn with the body, one attribute per listed feature,
/// - `REQUIRES`, the `FeatureSet` of the **same** list,
/// - a private `#[inline(never)]` trampoline `call` that calls the body,
/// - `call_checked`, a safe entry that returns `None` when the features are not in use, and
/// - `IMPL`, the `Impl` with the trampoline.
///
/// A literal that does not match its `Feature` is a compile error (`same_name`), so the attribute
/// and the mask cannot disagree. The expansion site must sit in a module with
/// `#![allow(unsafe_code)]` (ADR-0027 CD-9: `arch/` only). The trampoline's `unsafe` is sound because the resolver installs
/// the `Impl` only when `REQUIRES` is a subset of the enabled (detected and not disabled) features.
/// The body fn is a safe `#[target_feature]` fn, so safe intrinsics inside it need no `unsafe`;
/// the shared body that LLVM should vectorise is a separate `#[inline(always)]` fn.
#[macro_export]
macro_rules! kernel_impl {
    ($(#[$m:meta])* $vis:vis mod $name:ident [$($lit:literal => $feat:ident),+ $(,)?]
        ($($arg:ident : $ty:ty),* $(,)?) -> $ret:ty $body:block) => {
        $(#[$m])*
        #[allow(unused_imports)]
        $vis mod $name {
            use super::*;

            $( const _: () = assert!(
                $crate::same_name($lit, $crate::Feature::$feat.name()),
                "target_feature literal does not match its Feature",
            ); )+

            /// Features this implementation needs: the same list as the attributes below.
            pub const REQUIRES: $crate::FeatureSet = $crate::FeatureSet::of(&[$($crate::Feature::$feat),+]);

            $( #[target_feature(enable = $lit)] )+
            fn body($($arg: $ty),*) -> $ret $body

            /// The dispatch edge. Never inlined into a caller without the features. Private: it
            /// is reachable only through `IMPL` (resolver, `Impl::checked`) and `call_checked`.
            #[inline(never)]
            fn call($($arg: $ty),*) -> $ret {
                // SAFETY: `IMPL.requires` is `REQUIRES`, built from the same list as the
                // `target_feature` attributes on `body`; this fn runs only after the resolver
                // (or `Impl::checked`, or `call_checked`) saw all of them detected and not
                // disabled, so the instructions are supported.
                unsafe { body($($arg),*) }
            }

            /// Safe direct entry: runs the implementation when this process may use it.
            #[allow(dead_code)]
            pub fn call_checked($($arg: $ty),*) -> Option<$ret> {
                REQUIRES
                    .is_subset_of($crate::active().enabled.set())
                    .then(|| call($($arg),*))
            }

            /// The registry entry.
            pub const IMPL: $crate::Impl<fn($($ty),*) -> $ret> =
                $crate::Impl::new(stringify!($name), REQUIRES, call);
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feature::Feature;

    fn p(x: u32) -> u32 {
        x
    }
    fn a(x: u32) -> u32 {
        x + 1
    }
    fn b(x: u32) -> u32 {
        x + 2
    }
    type F = fn(u32) -> u32;
    static IMPLS: [Impl<F>; 2] = [
        Impl::new("a", FeatureSet::of(&[Feature::Avx2, Feature::Bmi2]), a),
        Impl::new("b", FeatureSet::of(&[Feature::Avx2]), b),
    ];
    static SPEC: KernelSpec<fn(u32) -> u32> = KernelSpec {
        id: "t",
        portable: p,
        impls: &IMPLS,
        min_len: 0,
    };

    #[test]
    fn first_fitting_impl_wins() {
        let en = |f: &[Feature]| Enabled::new_unchecked(FeatureSet::of(f));
        let all = en(&[Feature::Avx2, Feature::Bmi2]);
        assert_eq!((SPEC.resolve(all).0)(0), 1);
        let avx2 = en(&[Feature::Avx2]);
        assert_eq!(SPEC.resolve(avx2).1.name, "b");
        let none = SPEC.resolve(Enabled::none());
        assert!(none.1.is_portable());
        assert_eq!((none.0)(0), 0);
    }
}
