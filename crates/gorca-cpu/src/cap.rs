// SPDX-License-Identifier: GPL-3.0-or-later
//! `Cap<F>`: a zero-sized proof that feature `F` was detected and not disabled.
//!
//! The kernel table's soundness does not rest on it (a `#[target_feature]` function cannot be
//! coerced to a safe `fn` pointer, so `kernel_impl!` ties the attribute list to the `requires`
//! mask instead, ADR-0027 section 2.3). `Cap` is for code that wants a *direct, typed* call
//! outside the table: it is only obtainable through [`Cap::get`], which consults the resolved
//! state, and a function that takes `Cap<marker::Avx2>` documents in its signature that the
//! caller already proved AVX2.

use std::marker::PhantomData;

use crate::feature::FeatureMarker;
use crate::table::active;

/// Proof that `F` is in use on this process. Only [`Cap::get`] constructs one.
#[derive(Copy, Clone, Debug)]
pub struct Cap<F: FeatureMarker>(PhantomData<F>);

impl<F: FeatureMarker> Cap<F> {
    /// `Some` when the CPU has `F`, the OS enabled its state, and it is not disabled.
    #[must_use]
    pub fn get() -> Option<Cap<F>> {
        active()
            .enabled
            .contains(F::FEATURE)
            .then_some(Cap(PhantomData))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feature::marker;

    #[test]
    fn cap_follows_the_enabled_set() {
        let a = active();
        assert_eq!(
            Cap::<marker::Avx2>::get().is_some(),
            a.enabled.contains(crate::Feature::Avx2)
        );
        assert_eq!(
            Cap::<marker::Sse42>::get().is_some(),
            a.enabled.contains(crate::Feature::Sse42)
        );
    }
}
