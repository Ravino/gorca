// SPDX-License-Identifier: GPL-3.0-or-later
//! `Enabled`: the feature set a kernel may be selected against.
//!
//! A `#[target_feature]` trampoline is only sound on a CPU that has its features, so the safe
//! resolvers take this type instead of a caller-built `FeatureSet`. Outside the crate it can
//! only be produced by detection ([`Enabled::detected`]), by the disable filter
//! ([`crate::DisableList::enabled`]) or by narrowing an existing value ([`Enabled::restrict`]):
//! never widened, never invented.

use crate::detect::detect;
use crate::feature::{Feature, FeatureSet};

/// Features that are detected on this machine and not disabled.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Enabled(FeatureSet);

impl Enabled {
    /// Everything the hardware and the OS support, ignoring any disable list.
    #[must_use]
    pub fn detected() -> Enabled {
        Enabled(detect())
    }

    /// Nothing: every kernel resolves to its portable function.
    #[must_use]
    pub const fn none() -> Enabled {
        Enabled(FeatureSet::EMPTY)
    }

    /// Only the features that are also in `mask`.
    #[must_use]
    pub const fn restrict(self, mask: FeatureSet) -> Enabled {
        Enabled(self.0.intersection(mask))
    }

    #[must_use]
    pub const fn contains(self, f: Feature) -> bool {
        self.0.contains(f)
    }

    /// The members, as plain data (for reports).
    #[must_use]
    pub const fn set(self) -> FeatureSet {
        self.0
    }

    /// Crate-internal: the disable filter and tests that need an arbitrary mask. Never public.
    pub(crate) const fn new_unchecked(set: FeatureSet) -> Enabled {
        Enabled(set)
    }
}
