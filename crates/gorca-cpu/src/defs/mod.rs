// SPDX-License-Identifier: GPL-3.0-or-later
//! The kernels: for each, the safe portable function (the specification), the shared
//! `#[inline(always)]` body that the wide implementations in `arch/` recompile, its `SPEC`, and
//! the equivalence tests. No `unsafe` here.

pub(crate) mod common_prefix;
pub(crate) mod utf8_extend;
pub(crate) mod utf8_scalar_count;

#[cfg(test)]
pub(crate) mod testutil {
    use crate::registry::KernelSpec;

    /// Every implementation of `spec` the hardware can run, portable first. The disable list is
    /// ignored on purpose: equivalence is a property of the code, and the `GORCA_CPU_DISABLE`
    /// test job proves the portable path through the rest of the suite. On a CPU without the
    /// features only portable remains and the test degenerates to portable-vs-oracle.
    pub(crate) fn runnable<F: Copy>(spec: &KernelSpec<F>) -> Vec<(&'static str, F)> {
        let enabled = crate::Enabled::detected();
        let mut v = vec![(crate::PORTABLE, spec.portable)];
        for i in spec.impls {
            if let Some(f) = i.checked(enabled) {
                v.push((i.name, f));
            }
        }
        v
    }
}
